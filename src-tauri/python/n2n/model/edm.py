"""EDM diffusion (Karras et al., 2022, "Elucidating the Design Space ...").

Implements:
- Lognormal sigma sampling for training (P_mean, P_std).
- EDM preconditioning: c_skip, c_out, c_in, c_noise as functions of sigma,
  so the inner network sees magnitude-bounded inputs/outputs across all sigmas.
- ρ-parameterized noise schedule for sampling (ρ=7 by default).
- Heun-style 2nd-order deterministic sampler.

The "inner network" F_θ(c_in·x_noisy, c_noise(sigma), conditioning) is exposed
as a callable. The wrapper turns that into the EDM denoiser
    D(x; sigma, cond) = c_skip(sigma)·x + c_out(sigma)·F_θ(c_in(sigma)·x, c_noise(sigma), cond)
which the sampler uses directly.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Callable, Protocol

import torch
from torch import Tensor


@dataclass
class EDMConfig:
    sigma_data: float = 0.5  # rms of clean target. APH-normalized x ∈ [-1, 1] → ~0.5 is sensible default
    sigma_min: float = 0.002
    sigma_max: float = 80.0
    p_mean: float = -1.2  # training sigma ~ lognormal(P_mean, P_std)
    p_std: float = 1.2
    rho: float = 7.0  # noise schedule shape


class InnerNet(Protocol):
    """The denoiser F_θ. Sees preconditioned inputs and emits a target prediction."""

    def __call__(self, x_in: Tensor, c_noise: Tensor, cond: dict) -> Tensor: ...


def _broadcast_sigma(sigma: Tensor, x: Tensor) -> Tensor:
    """Reshape sigma (B,) so it broadcasts against x (B, ...)."""
    return sigma.view(-1, *([1] * (x.dim() - 1)))


def precondition(sigma: Tensor, x_noisy: Tensor, cfg: EDMConfig) -> tuple[Tensor, Tensor, Tensor, Tensor]:
    """Returns (c_skip, c_out, c_in, c_noise), all broadcastable against x_noisy."""
    sigma_b = _broadcast_sigma(sigma, x_noisy)
    sd2 = cfg.sigma_data ** 2
    c_skip = sd2 / (sigma_b ** 2 + sd2)
    c_out = sigma_b * cfg.sigma_data / torch.sqrt(sigma_b ** 2 + sd2)
    c_in = 1.0 / torch.sqrt(sigma_b ** 2 + sd2)
    c_noise = 0.25 * torch.log(sigma)  # not broadcast — passed to net as (B,)
    return c_skip, c_out, c_in, c_noise


def denoise(
    inner: InnerNet,
    x_noisy: Tensor,
    sigma: Tensor,
    cond: dict,
    cfg: EDMConfig,
) -> Tensor:
    """EDM denoiser D(x; sigma, cond)."""
    c_skip, c_out, c_in, c_noise = precondition(sigma, x_noisy, cfg)
    f = inner(c_in * x_noisy, c_noise, cond)
    return c_skip * x_noisy + c_out * f


def sample_training_sigma(
    batch_size: int,
    cfg: EDMConfig,
    device: torch.device | str = "cpu",
) -> Tensor:
    """sigma ~ exp(N(P_mean, P_std))."""
    log_sigma = cfg.p_mean + cfg.p_std * torch.randn(batch_size, device=device)
    return log_sigma.exp()


def edm_loss_weight(sigma: Tensor, cfg: EDMConfig) -> Tensor:
    """λ(sigma) = (sigma^2 + sigma_data^2) / (sigma · sigma_data)^2 — Karras Eq. 8."""
    sd2 = cfg.sigma_data ** 2
    return (sigma ** 2 + sd2) / (sigma * cfg.sigma_data) ** 2


def edm_training_step(
    inner: InnerNet,
    x_clean: Tensor,
    cond: dict,
    cfg: EDMConfig,
    loss_fn: Callable[[Tensor, Tensor], Tensor] | None = None,
) -> tuple[Tensor, Tensor, Tensor]:
    """One denoising step of training.

    Returns:
        (loss, denoised, sigma) — denoised is D(x_noisy; sigma, cond).

    If loss_fn is None, falls back to weighted MSE per Karras Eq. 8. Otherwise the
    caller's loss_fn is applied directly to (denoised, x_clean) and the EDM weight
    is applied externally if desired. (We use APH loss without the EDM weight.)
    """
    sigma = sample_training_sigma(x_clean.size(0), cfg, device=x_clean.device)
    sigma_b = _broadcast_sigma(sigma, x_clean)
    noise = torch.randn_like(x_clean) * sigma_b
    x_noisy = x_clean + noise

    denoised = denoise(inner, x_noisy, sigma, cond, cfg)

    if loss_fn is None:
        weight = _broadcast_sigma(edm_loss_weight(sigma, cfg), x_clean)
        loss = (weight * (denoised - x_clean) ** 2).mean()
    else:
        loss = loss_fn(denoised, x_clean)

    return loss, denoised, sigma


# ---------------------------------------------------------------------------
# Sampling
# ---------------------------------------------------------------------------


def rho_schedule(num_steps: int, cfg: EDMConfig, device: torch.device | str = "cpu") -> Tensor:
    """ρ-parameterized sigma schedule from sigma_max → sigma_min, plus a final 0.

    Shape: (num_steps + 1,). Last element is exactly 0 (clean sample).
    """
    i = torch.arange(num_steps, device=device, dtype=torch.float32)
    inv_rho = 1.0 / cfg.rho
    sigmas = (
        cfg.sigma_max ** inv_rho
        + (i / max(num_steps - 1, 1)) * (cfg.sigma_min ** inv_rho - cfg.sigma_max ** inv_rho)
    ) ** cfg.rho
    return torch.cat([sigmas, sigmas.new_zeros(1)])


@torch.no_grad()
def sample_heun(
    inner: InnerNet,
    cond: dict,
    shape: tuple[int, ...],
    cfg: EDMConfig,
    num_steps: int = 5,
    device: torch.device | str = "cuda",
    dtype: torch.dtype = torch.float32,
    generator: torch.Generator | None = None,
) -> Tensor:
    """Heun-style 2nd-order deterministic sampler. Returns x_clean of given shape."""
    sigmas = rho_schedule(num_steps, cfg, device=device).to(dtype)
    x = sigmas[0] * torch.randn(shape, device=device, dtype=dtype, generator=generator)

    for i in range(num_steps):
        sigma_cur = sigmas[i]
        sigma_next = sigmas[i + 1]
        sigma_b = sigma_cur.expand(shape[0])
        d_cur = (x - denoise(inner, x, sigma_b, cond, cfg)) / sigma_cur
        x_next = x + (sigma_next - sigma_cur) * d_cur

        # Heun correction (skip on the last step where sigma_next = 0).
        if sigma_next > 0:
            sigma_b_next = sigma_next.expand(shape[0])
            d_next = (x_next - denoise(inner, x_next, sigma_b_next, cond, cfg)) / sigma_next
            x_next = x + (sigma_next - sigma_cur) * 0.5 * (d_cur + d_next)

        x = x_next

    return x
