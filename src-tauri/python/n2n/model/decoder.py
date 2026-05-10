"""EDGE-style transformer decoder for diffusion-based drum transcription.

The "inner network" F_θ that EDM preconditioning wraps. Architecture:

  target tokens  ──[self-attn → cross-attn-mel → cross-attn-mert → FFN]× L_dec──> output
       ↑                            ↑                  ↑
  (B, N, 14)                  mel encoder         MERT encoder
                              (mel-spec in)      (MERT layer-7 in)
                                  ↑                  ↑
                            log-mel (B,128,T_m)  MERT (B,T_q,768)

  + FiLM gates from timestep embedding (sinusoidal(c_noise) → MLP) at every
    decoder layer (modulating self-attn input and FFN input).

Positional encoding: RoPE applied to Q AND K in every attention layer (target
self-attn, both decoder cross-attns, and both conditioning encoders' self-attn).
Positions live in a single shared time grid where 1 unit = 10 ms (the target
frame hop). Mel and target tokens index into the grid as integers (their hop
is 10 ms by construction); MERT tokens use scale = 100/75 = 4/3 because MERT
is at 75 Hz. With Q and K rotated in this shared grid, the dot product of any
attention pair encodes their relative time difference — including across the
target↔MERT boundary, which the v2-v4 RoPE attempt got wrong by stripping
positional information from cross-attention entirely.

Mel and MERT are kept as separate cross-attention streams (not concat-then-
project) so the per-stream dropout regimes from §3.3 of the paper can be added
later without restructuring the model.
"""

from __future__ import annotations

import math
from dataclasses import dataclass

import torch
import torch.nn.functional as F
from torch import Tensor, nn

# Frame-rate ratio between target/mel (100 Hz, 10 ms) and MERT (75 Hz, 13.33 ms).
# Used to express MERT positions in the shared 10-ms time grid so RoPE rotation
# on a target Q and MERT K with the same wall-clock time produces equal angles.
MERT_POS_SCALE = 100.0 / 75.0


# ---------------------------------------------------------------------------
# Building blocks
# ---------------------------------------------------------------------------


def sinusoidal_embedding(t: Tensor, dim: int) -> Tensor:
    """Sinusoidal embedding of a 1-D tensor of arbitrary scalar values.

    t: (B,) float. Returns (B, dim).
    Used for diffusion timesteps (c_noise) only — sequence positions go through
    RoPE applied inside attention, not added to embeddings.
    """
    half = dim // 2
    freqs = torch.exp(
        -math.log(10_000.0) * torch.arange(half, device=t.device, dtype=torch.float32) / half
    )
    args = t.float().unsqueeze(-1) * freqs.unsqueeze(0)
    emb = torch.cat([args.sin(), args.cos()], dim=-1)
    if emb.size(-1) < dim:
        emb = nn.functional.pad(emb, (0, dim - emb.size(-1)))
    return emb


# ---------------------------------------------------------------------------
# RoPE
# ---------------------------------------------------------------------------


class RotaryEmbedding(nn.Module):
    """LLaMA-style RoPE. Holds the frequency basis; cos/sin are computed from
    a positions vector at forward time. Positions are float — fractional values
    work, which is what we need for the 4/3-stride MERT track."""

    def __init__(self, head_dim: int, base: float = 10000.0) -> None:
        super().__init__()
        if head_dim % 2 != 0:
            raise ValueError(f"RoPE head_dim must be even, got {head_dim}")
        inv_freq = 1.0 / (
            base ** (torch.arange(0, head_dim, 2, dtype=torch.float32) / head_dim)
        )
        self.register_buffer("inv_freq", inv_freq, persistent=False)
        self.head_dim = head_dim

    def cos_sin(self, positions: Tensor) -> tuple[Tensor, Tensor]:
        """positions: (T,) float. Returns (cos, sin), each (T, head_dim)."""
        # (T, head_dim/2)
        freqs = positions.float().unsqueeze(-1) * self.inv_freq.to(positions.device).unsqueeze(0)
        # Duplicate across the second half so rotate_half pairs interleave correctly.
        emb = torch.cat([freqs, freqs], dim=-1)
        return emb.cos(), emb.sin()


def _rotate_half(x: Tensor) -> Tensor:
    x1, x2 = x.chunk(2, dim=-1)
    return torch.cat([-x2, x1], dim=-1)


def apply_rope(x: Tensor, cos: Tensor, sin: Tensor) -> Tensor:
    """x: (B, H, T, Dh). cos, sin: (T, Dh). Broadcasts over (B, H)."""
    cos = cos.to(x.dtype)[None, None, :, :]
    sin = sin.to(x.dtype)[None, None, :, :]
    return x * cos + _rotate_half(x) * sin


class RoPEAttention(nn.Module):
    """Multi-head attention with RoPE applied to Q and K. Supports cross-
    attention (Q, K, V come from different streams with separate position
    grids) and self-attention (pass the same tensor and positions for Q/K/V).

    Uses F.scaled_dot_product_attention so PyTorch can dispatch to Flash /
    memory-efficient kernels when the inputs allow.
    """

    def __init__(
        self,
        d_model: int,
        n_heads: int,
        dropout: float = 0.0,
        rope_base: float = 10000.0,
    ) -> None:
        super().__init__()
        if d_model % n_heads != 0:
            raise ValueError(f"d_model={d_model} not divisible by n_heads={n_heads}")
        self.d_model = d_model
        self.n_heads = n_heads
        self.head_dim = d_model // n_heads
        self.q_proj = nn.Linear(d_model, d_model)
        self.k_proj = nn.Linear(d_model, d_model)
        self.v_proj = nn.Linear(d_model, d_model)
        self.out_proj = nn.Linear(d_model, d_model)
        self.dropout = dropout
        self.rope = RotaryEmbedding(self.head_dim, base=rope_base)

    def forward(
        self,
        q_in: Tensor,                       # (B, Tq, D)
        k_in: Tensor,                       # (B, Tk, D)
        v_in: Tensor,                       # (B, Tk, D)
        q_positions: Tensor,                # (Tq,) float
        k_positions: Tensor,                # (Tk,) float
        key_padding_mask: Tensor | None = None,  # (B, Tk) bool, True = pad
    ) -> Tensor:
        B = q_in.size(0)
        Tq = q_in.size(1)
        Tk = k_in.size(1)
        H = self.n_heads
        Dh = self.head_dim

        q = self.q_proj(q_in).view(B, Tq, H, Dh).transpose(1, 2)
        k = self.k_proj(k_in).view(B, Tk, H, Dh).transpose(1, 2)
        v = self.v_proj(v_in).view(B, Tk, H, Dh).transpose(1, 2)

        cos_q, sin_q = self.rope.cos_sin(q_positions)
        cos_k, sin_k = self.rope.cos_sin(k_positions)
        q = apply_rope(q, cos_q, sin_q)
        k = apply_rope(k, cos_k, sin_k)

        attn_mask: Tensor | None = None
        if key_padding_mask is not None:
            # SDPA boolean attn_mask: True = keep, False = mask out. Our
            # key_padding_mask convention is True = pad → mask out, so invert.
            # (B, 1, 1, Tk) broadcasts over heads and Q.
            attn_mask = (~key_padding_mask)[:, None, None, :]

        out = F.scaled_dot_product_attention(
            q, k, v,
            attn_mask=attn_mask,
            dropout_p=self.dropout if self.training else 0.0,
            is_causal=False,
        )
        out = out.transpose(1, 2).reshape(B, Tq, H * Dh)
        return self.out_proj(out)


# ---------------------------------------------------------------------------
# FFN, FiLM, encoder/decoder layers
# ---------------------------------------------------------------------------


class FeedForward(nn.Module):
    def __init__(self, d_model: int, d_ffn: int, dropout: float = 0.0) -> None:
        super().__init__()
        self.net = nn.Sequential(
            nn.Linear(d_model, d_ffn),
            nn.GELU(),
            nn.Dropout(dropout),
            nn.Linear(d_ffn, d_model),
        )

    def forward(self, x: Tensor) -> Tensor:
        return self.net(x)


class EncoderLayer(nn.Module):
    """Pre-norm transformer encoder layer with RoPE self-attention."""

    def __init__(self, d_model: int, n_heads: int, d_ffn: int, dropout: float = 0.0) -> None:
        super().__init__()
        self.norm1 = nn.LayerNorm(d_model)
        self.attn = RoPEAttention(d_model, n_heads, dropout=dropout)
        self.norm2 = nn.LayerNorm(d_model)
        self.ffn = FeedForward(d_model, d_ffn, dropout)

    def forward(
        self,
        x: Tensor,
        positions: Tensor,
        key_padding_mask: Tensor | None = None,
    ) -> Tensor:
        h = self.norm1(x)
        a = self.attn(
            h, h, h,
            q_positions=positions, k_positions=positions,
            key_padding_mask=key_padding_mask,
        )
        x = x + a
        x = x + self.ffn(self.norm2(x))
        return x


class ConditioningEncoder(nn.Module):
    """Linear in-projection + N RoPE transformer encoder layers."""

    def __init__(
        self,
        in_dim: int,
        d_model: int,
        n_heads: int,
        d_ffn: int,
        n_layers: int,
        dropout: float = 0.0,
    ) -> None:
        super().__init__()
        self.in_proj = nn.Linear(in_dim, d_model)
        self.layers = nn.ModuleList(
            [EncoderLayer(d_model, n_heads, d_ffn, dropout) for _ in range(n_layers)]
        )
        self.norm = nn.LayerNorm(d_model)

    def forward(
        self,
        x: Tensor,
        positions: Tensor,
        key_padding_mask: Tensor | None = None,
    ) -> Tensor:
        """x: (B, T, in_dim) → (B, T, d_model). positions: (T,) in shared 10-ms grid."""
        h = self.in_proj(x)
        for layer in self.layers:
            h = layer(h, positions=positions, key_padding_mask=key_padding_mask)
        return self.norm(h)


class FiLM(nn.Module):
    """Feature-wise Linear Modulation: x · (1 + gamma) + beta."""

    def __init__(self, d_cond: int, d_model: int) -> None:
        super().__init__()
        self.proj = nn.Linear(d_cond, 2 * d_model)

    def forward(self, x: Tensor, cond: Tensor) -> Tensor:
        """x: (B, T, d_model). cond: (B, d_cond) → broadcast over T."""
        gamma, beta = self.proj(cond).unsqueeze(1).chunk(2, dim=-1)
        return x * (1.0 + gamma) + beta


class DecoderLayer(nn.Module):
    """Pre-norm decoder layer: self-attn → cross-attn(mel) → cross-attn(mert) → FFN.

    All three attention sub-blocks use RoPE on Q+K. FiLM gates from the
    timestep embedding modulate the input to self-attn and to FFN.
    """

    def __init__(
        self,
        d_model: int,
        n_heads: int,
        d_ffn: int,
        d_time: int,
        dropout: float = 0.0,
    ) -> None:
        super().__init__()
        self.norm_self = nn.LayerNorm(d_model)
        self.film_self = FiLM(d_time, d_model)
        self.self_attn = RoPEAttention(d_model, n_heads, dropout=dropout)

        self.norm_cross_mel = nn.LayerNorm(d_model)
        self.cross_attn_mel = RoPEAttention(d_model, n_heads, dropout=dropout)

        self.norm_cross_mert = nn.LayerNorm(d_model)
        self.cross_attn_mert = RoPEAttention(d_model, n_heads, dropout=dropout)

        self.norm_ffn = nn.LayerNorm(d_model)
        self.film_ffn = FiLM(d_time, d_model)
        self.ffn = FeedForward(d_model, d_ffn, dropout)

    def forward(
        self,
        x: Tensor,
        mel: Tensor,
        mert: Tensor,
        t_emb: Tensor,
        target_pos: Tensor,
        mel_pos: Tensor,
        mert_pos: Tensor,
        target_kpm: Tensor | None = None,
        mel_kpm: Tensor | None = None,
        mert_kpm: Tensor | None = None,
    ) -> Tensor:
        h = self.film_self(self.norm_self(x), t_emb)
        a = self.self_attn(
            h, h, h,
            q_positions=target_pos, k_positions=target_pos,
            key_padding_mask=target_kpm,
        )
        x = x + a

        h = self.norm_cross_mel(x)
        a = self.cross_attn_mel(
            h, mel, mel,
            q_positions=target_pos, k_positions=mel_pos,
            key_padding_mask=mel_kpm,
        )
        x = x + a

        h = self.norm_cross_mert(x)
        a = self.cross_attn_mert(
            h, mert, mert,
            q_positions=target_pos, k_positions=mert_pos,
            key_padding_mask=mert_kpm,
        )
        x = x + a

        h = self.film_ffn(self.norm_ffn(x), t_emb)
        x = x + self.ffn(h)
        return x


# ---------------------------------------------------------------------------
# Full model
# ---------------------------------------------------------------------------


@dataclass
class N2NConfig:
    n_drums: int = 4  # D — v6 onward; was 7 in paper / runs 000-004
    n_axes: int = 1  # v8+: onset only. Pre-v8 was 2 (onset + velocity).
    d_model: int = 512
    n_heads: int = 8
    d_ffn: int = 2048
    n_decoder_layers: int = 6
    n_mel_encoder_layers: int = 2
    n_mert_encoder_layers: int = 2
    mel_in_dim: int = 128  # log-mel bins
    mert_in_dim: int = 768  # MERT-95M hidden size
    d_time: int = 256  # timestep embedding width
    dropout: float = 0.0

    @property
    def n_target_channels(self) -> int:
        return self.n_drums * self.n_axes


class N2NDecoder(nn.Module):
    """Inner denoiser F_θ for EDM. Predicts target tensor (B, N, D, A) where
    A = cfg.n_axes (1 for onset-only, was 2 for onset+velocity pre-v8).

    Optional padding masks ride in `cond`:
        cond["target_kpm"]: (B, N) bool, True = pad
        cond["mel_kpm"]:    (B, T_mel) bool
        cond["mert_kpm"]:   (B, T_mert) bool
    Missing keys → no masking (full sequences).
    """

    def __init__(self, cfg: N2NConfig) -> None:
        super().__init__()
        self.cfg = cfg

        self.mel_encoder = ConditioningEncoder(
            cfg.mel_in_dim, cfg.d_model, cfg.n_heads, cfg.d_ffn,
            cfg.n_mel_encoder_layers, cfg.dropout,
        )
        self.mert_encoder = ConditioningEncoder(
            cfg.mert_in_dim, cfg.d_model, cfg.n_heads, cfg.d_ffn,
            cfg.n_mert_encoder_layers, cfg.dropout,
        )

        self.time_mlp = nn.Sequential(
            nn.Linear(cfg.d_time, cfg.d_time * 4),
            nn.GELU(),
            nn.Linear(cfg.d_time * 4, cfg.d_time),
        )

        self.target_in = nn.Linear(cfg.n_target_channels, cfg.d_model)
        self.layers = nn.ModuleList(
            [
                DecoderLayer(cfg.d_model, cfg.n_heads, cfg.d_ffn, cfg.d_time, cfg.dropout)
                for _ in range(cfg.n_decoder_layers)
            ]
        )
        self.norm_out = nn.LayerNorm(cfg.d_model)
        self.target_out = nn.Linear(cfg.d_model, cfg.n_target_channels)

    def forward(
        self,
        x_in: Tensor,            # (B, N, D, A) — preconditioned noisy target
        c_noise: Tensor,         # (B,)
        cond: dict,              # see class docstring for keys
    ) -> Tensor:
        """Returns predicted clean-target shaped like x_in."""
        B, N, D, A = x_in.shape
        device = x_in.device

        mel_in = cond["mel"].transpose(1, 2)        # (B, T_mel, n_mels)
        mert_in = cond["mert"]                      # (B, T_mert, mert_dim)
        T_mel = mel_in.size(1)
        T_mert = mert_in.size(1)

        target_kpm = cond.get("target_kpm")
        mel_kpm = cond.get("mel_kpm")
        mert_kpm = cond.get("mert_kpm")

        # Positions in the shared 10-ms grid.
        target_pos = torch.arange(N, device=device, dtype=torch.float32)
        mel_pos = torch.arange(T_mel, device=device, dtype=torch.float32)
        mert_pos = torch.arange(T_mert, device=device, dtype=torch.float32) * MERT_POS_SCALE

        mel = self.mel_encoder(mel_in, positions=mel_pos, key_padding_mask=mel_kpm)
        mert = self.mert_encoder(mert_in, positions=mert_pos, key_padding_mask=mert_kpm)

        t_emb = sinusoidal_embedding(c_noise, self.cfg.d_time)
        t_emb = self.time_mlp(t_emb)

        x = x_in.reshape(B, N, D * A)
        h = self.target_in(x)

        for layer in self.layers:
            h = layer(
                h,
                mel=mel, mert=mert, t_emb=t_emb,
                target_pos=target_pos, mel_pos=mel_pos, mert_pos=mert_pos,
                target_kpm=target_kpm, mel_kpm=mel_kpm, mert_kpm=mert_kpm,
            )

        h = self.norm_out(h)
        out = self.target_out(h)
        return out.view(B, N, D, A)


def count_params(m: nn.Module) -> int:
    return sum(p.numel() for p in m.parameters())
