# Standalone SpeexDSP SRC

Source: https://github.com/xiph/speexdsp/tree/1b28a0f61bc31162979e1f26f3981fc3637095c8
Release: SpeexDSP-1.2.1 (BSD-3-Clause).

Unmodified upstream files:

- `libspeexdsp/resample.c`
- `libspeexdsp/arch.h`
- `include/speex/speex_resampler.h`
- `COPYING` (stored here as `LICENSE`)

Built with `OUTSIDE_SPEEX`, `FLOATING_POINT`, and `RANDOM_PREFIX=remdia_src`.
No Speex codec, echo canceller, jitter buffer or other DSP module is included.
The separate `fallback.audio.resample` plugin is packaged only by the Android
compatibility AAR build. `--system-only` excludes it. Windows/macOS staging
continues to use their system resamplers and does not distribute this plugin.
