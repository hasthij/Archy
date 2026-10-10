"""Generates Archy's short UI sounds (no dependencies).

    python assets/make_sounds.py  ->  assets/clip.wav, assets/voice.wav

clip.wav:  two quick rising notes, played when a clip is saved.
voice.wav: one soft blip, played the moment a voice phrase is heard.
"""
import math
import os
import struct

HERE = os.path.dirname(os.path.abspath(__file__))
RATE = 44100


def tone(freq, seconds, volume=0.35):
    n = int(RATE * seconds)
    out = []
    for i in range(n):
        t = i / RATE
        # Fast attack, smooth exponential decay: a soft "pluck", not a beep.
        env = min(1.0, t / 0.004) * math.exp(-t * 18)
        # A touch of the octave makes it rounder.
        s = math.sin(2 * math.pi * freq * t) + 0.25 * math.sin(4 * math.pi * freq * t)
        out.append(volume * env * s / 1.25)
    return out


def write(name, samples):
    data = b"".join(struct.pack("<h", max(-32767, min(32767, int(s * 32767)))) for s in samples)
    header = b"RIFF" + struct.pack("<I", 36 + len(data)) + b"WAVE"
    fmt = b"fmt " + struct.pack("<IHHIIHH", 16, 1, 1, RATE, RATE * 2, 2, 16)
    with open(os.path.join(HERE, name), "wb") as f:
        f.write(header + fmt + b"data" + struct.pack("<I", len(data)) + data)


if __name__ == "__main__":
    gap = [0.0] * int(RATE * 0.02)
    write("clip.wav", tone(880, 0.12) + gap + tone(1318.5, 0.28))  # A5 -> E6
    write("voice.wav", tone(1046.5, 0.16, 0.25))  # C6
    print("wrote clip.wav, voice.wav")
