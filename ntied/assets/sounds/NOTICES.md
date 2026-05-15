# Sound effect attribution

These notification tones are from
[akx/Notifications](https://github.com/akx/Notifications) (commit pinned by
the file copies in this directory). The author dual-licenses them under
**CC Attribution 3.0 Unported** and **CC0 Public Domain** — choose either.

| File                  | Source name             |
| --------------------- | ----------------------- |
| `new_message.wav`     | `Polite.wav`            |
| `peer_added.wav`      | `Chord2.wav`            |
| `call_ended.wav`      | `Whistleronic-Down.wav` |
| `call_rejected.wav`   | `Sharp.wav`             |
| `ringtone.wav`        | `Reverie.wav`           |
| `call_connected.wav`  | `Chord.wav`             |

To swap a sound, drop a replacement WAV with the same filename here. The
loader expects 16-bit signed PCM, mono, 44100 Hz (the format akx ships).
Other formats need a one-time edit in `src/audio/sound_effect.rs`.
