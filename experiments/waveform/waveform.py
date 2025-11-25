"""
Rekordbox-style 3-band waveform renderer.
"""

import numpy as np
import json
import base64
import librosa
import os
from PIL import Image
from scipy import signal

# Band configuration - rekordbox style
BANDS = {
    'low': {
        'color': np.array([0x00, 0x55, 0xe2]),  # Blue
        'height_scale': 1.0,  # Full height
    },
    'mid': {
        'color': np.array([0xf2, 0xaa, 0x3c]),  # Orange/Amber
        'height_scale': 1.0,
    },
    'high': {
        'color': np.array([0xff, 0xff, 0xff]),  # White/Cream
        'height_scale': 1.0,
    },
}


def analyze_audio_3band(audio, sample_rate, pixels_per_second=150):
    """
    Analyze audio into 3 frequency bands using proper filtering.
    """
    samples_per_pixel = sample_rate // pixels_per_second
    
    # Band boundaries (Hz)
    LOW_END = 250
    MID_END = 4000
    
    nyquist = sample_rate / 2
    
    # Create butterworth filters (order 2 = 12dB/oct)
    sos_low = signal.butter(2, LOW_END / nyquist, btype='low', output='sos')
    sos_mid = signal.butter(2, [LOW_END / nyquist, MID_END / nyquist], btype='band', output='sos')
    sos_high = signal.butter(2, MID_END / nyquist, btype='high', output='sos')
    
    # Filter the audio
    low_audio = signal.sosfilt(sos_low, audio)
    mid_audio = signal.sosfilt(sos_mid, audio)
    high_audio = signal.sosfilt(sos_high, audio)
    
    # Compute envelopes (peak per column)
    num_columns = len(audio) // samples_per_pixel
    
    def get_envelope(filtered_audio):
        envelope = np.zeros(num_columns)
        for i in range(num_columns):
            start = i * samples_per_pixel
            end = start + samples_per_pixel
            if end <= len(filtered_audio):
                chunk = filtered_audio[start:end]
                envelope[i] = np.max(np.abs(chunk))
        return envelope
    
    envelopes = {
        'low': get_envelope(low_audio),
        'mid': get_envelope(mid_audio),
        'high': get_envelope(high_audio),
    }
    
    # Normalize each band to its 98th percentile
    for name in envelopes:
        env = envelopes[name]
        p98 = np.percentile(env, 98)
        if p98 > 0:
            env = env / p98
        envelopes[name] = np.clip(env, 0, 1)
    
    # Apply gamma compression for better visual dynamics
    gamma = 0.55
    for name in envelopes:
        envelopes[name] = np.power(envelopes[name], gamma)
    
    return envelopes


def render_3band(envelopes, width, height):
    """
    Render 3-band waveform - rekordbox style.
    
    Key insight: Each band has INDEPENDENT height.
    We draw in order: low (blue) -> mid (orange) -> high (white)
    Later bands OVERWRITE earlier bands where they have content.
    
    This creates the layered look where:
    - Blue shows in center when bass is dominant
    - Orange shows where mids extend beyond bass
    - White shows at the peaks where highs are present
    """
    center_y = height // 2
    max_half = (height // 2) - 4
    
    # Final image
    image = np.zeros((height, width, 3), dtype=np.uint8)
    
    num_points = len(envelopes['low'])
    
    # Draw bands in order: low -> mid -> high
    # Each band overwrites the previous where it has content
    for name in ['low', 'mid', 'high']:
        config = BANDS[name]
        envelope = envelopes[name]
        color = config['color']
        
        for x in range(width):
            # Map pixel to envelope index
            idx = int(x * num_points / width)
            if idx >= len(envelope):
                continue
            
            env_val = envelope[idx]
            half_h = int(env_val * max_half * config['height_scale'])
            
            if half_h < 1:
                continue
            
            # Draw symmetric bar from center
            y_top = center_y - half_h
            y_bot = center_y + half_h
            
            # Clamp to image bounds
            y_top = max(0, y_top)
            y_bot = min(height, y_bot)
            
            # Draw this band's color (overwrites previous bands)
            image[y_top:y_bot, x] = color
    
    return image


if __name__ == "__main__":
    output_dir = os.path.dirname(__file__)
    song_dir = os.path.join(os.path.dirname(output_dir), "songs")
    
    # Pick a song
    song_name = "Mild Minds - TEARDROPS.mp3"
    song_path = os.path.join(song_dir, song_name)
    
    if not os.path.exists(song_path):
        # Try to find any mp3
        files = [f for f in os.listdir(song_dir) if f.endswith('.mp3')]
        if files:
            song_path = os.path.join(song_dir, files[0])
            song_name = files[0]
    
    print(f"Loading {song_name}...")
    
    # Load 5 seconds from middle
    total_duration = librosa.get_duration(path=song_path)
    duration = 5
    start_time = (total_duration - duration) / 2
    
    audio, sr = librosa.load(song_path, sr=None, offset=start_time, duration=duration, mono=True)
    
    # Normalize audio
    audio = audio / (np.max(np.abs(audio)) + 0.001)
    
    print(f"Analyzing {duration}s of audio (sr={sr})...")
    envelopes = analyze_audio_3band(audio, sr)
    
    # Debug: print envelope stats
    for name in ['low', 'mid', 'high']:
        env = envelopes[name]
        print(f"  {name}: min={env.min():.3f}, max={env.max():.3f}, mean={env.mean():.3f}")
    
    print("Rendering 3-band waveform...")
    img = render_3band(envelopes, 1500, 200)
    
    output_path = os.path.join(output_dir, "waveform_3band_v8.png")
    Image.fromarray(img).save(output_path)
    print(f"Saved {output_path}")
