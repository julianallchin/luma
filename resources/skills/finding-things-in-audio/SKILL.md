---
name: finding-things-in-audio
description: The shared Python detective kit. How to measure what the feature tags can't tell you — envelopes, bass modulation, brightness vs noisiness, snare hardness, real silence, fake drops. Genre skills say WHAT to find; this is HOW.
---
# Finding things in audio

The tags and features are a sketch. When a moment matters, measure it yourself.
Everything here runs in your Python surface. Always plot what you measure and
look at it — the plot comes back as an image, and your eyes are better than a
threshold.

## The amplitude envelope

The basic move. How loud is this stem, over time?

```python
import numpy as np
from scipy.signal import hilbert, butter, sosfiltfilt

sig = luma.audio.stems["bass"]        # or "drums", "vocals", "other", or luma.audio.mix
x = sig.values.mean(axis=1)           # mono
t = sig.times_s
sr = 1.0 / float(np.median(np.diff(t)))

env = np.abs(hilbert(x[::8]))         # decimate first; full-rate hilbert is wasteful
fs = sr / 8
env_t = t[::8][: len(env)]
```

## Bass modulation (wubs, and everything like them)

Band-limit the envelope to modulation rates. 0.5–8 Hz is where rhythmic bass
movement lives: below is section dynamics, above is timbre.

```python
sos = butter(4, [0.5, 8.0], btype="band", fs=fs, output="sos")
lfo = sosfiltfilt(sos, env - env.mean())
```

Per phrase (4 or 8 bars, from `features.bars.starts_s`):
- **Rate**: dominant frequency of `lfo` (FFT peak, or mean spacing from
  `scipy.signal.find_peaks`). Convert to a musical unit: `rate * 60 / bpm` beats.
  2.33 Hz at 140 BPM is triplet-eighths — a nameable rhythm.
- **Onsets**: `find_peaks(lfo)` gives the actual accent times. They are often
  off-grid. That's the point.
- **Depth**: peak-to-trough ratio. Deep and gated wants on/off lighting; shallow
  wants a breathe.

Stem separation smears aggressive bass into "other" — cross-check the mix when
a phrase looks emptier than it sounds.

## Character: bright, noisy, warm

Compare two sections (two drops, drop vs breakdown) with two numbers per section:

```python
from scipy.signal import stft
f, tt, Z = stft(x_section, fs=sr, nperseg=2048)
mag = np.abs(Z)
centroid = (f[:, None] * mag).sum(0) / (mag.sum(0) + 1e-9)   # brightness
flatness = np.exp(np.log(mag + 1e-9).mean(0)) / (mag.mean(0) + 1e-9)  # noisiness
```

Higher centroid = brighter, screechier. Higher flatness = noisier, more
distorted. A second drop with clearly higher flatness than the first is a track
saying "now it gets ugly" — genre skills tell you what to do with that.

## How hard does it hit

Snare (or any onset) hardness: take `features.drum_onsets["snare"]` times, read
the mix envelope at each, and compare the onset peak to the surrounding second
of audio. A snare 12 dB over its surroundings is artillery; 3 dB is texture.
This is how you tell punch-you halftime from flowy halftime at the same BPM.

## Real silence and fake drops

- **Silence**: RMS of the mix in short windows. Where it falls to the track's
  noise floor for more than a beat, the music stopped. Lights follow.
- **Fake drops**: at the end of a build, look at the first bar after: did bass
  energy actually arrive (bass-stem RMS jump), or did everything cut? A cut,
  a filtered stall, or another riser = feint. Check before you spend the hit.

## Phrases

Diff `features.bars.intensity` between consecutive bars; big jumps land on a
consistent multiple — 8, 16, or 32. That multiple is the phrase, and phrase
lines are where wholesale changes belong.

## Vocals

`vocal_lead` / `vocal_chop` tags say where; the vocal stem envelope says how
much. A cappella moments (vocal energy high, everything else at the floor) are
gift moments — genre skills spend them differently, but always find them.
