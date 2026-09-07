"""Log-mel spectrogram frontend.

Paper config: 44.1 kHz audio, 441-sample hop (10 ms frames), 128 mel bins.
Cheap enough to run on GPU at training time — no need to precache.
"""

from __future__ import annotations

import torch
import torchaudio
from torch import Tensor, nn


class LogMel(nn.Module):
    def __init__(
        self,
        sample_rate: int = 44_100,
        n_fft: int = 2048,
        hop_length: int = 441,
        n_mels: int = 128,
        f_min: float = 20.0,
        f_max: float | None = None,
        eps: float = 1e-5,
    ) -> None:
        super().__init__()
        self.eps = eps
        self.mel = torchaudio.transforms.MelSpectrogram(
            sample_rate=sample_rate,
            n_fft=n_fft,
            hop_length=hop_length,
            n_mels=n_mels,
            f_min=f_min,
            f_max=f_max if f_max is not None else sample_rate / 2,
            power=2.0,
            center=True,
        )

    def forward(self, audio: Tensor) -> Tensor:
        """audio: (B, T) or (B, 1, T) → (B, n_mels, frames). Returns log10 power."""
        if audio.dim() == 3 and audio.size(1) == 1:
            audio = audio.squeeze(1)
        spec = self.mel(audio)
        return torch.log10(spec + self.eps)
