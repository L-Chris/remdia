# Additional synthetic codec fixtures

`aac-surround.aac` contains four AAC-LC access units at 48 kHz and 5.1 channels.
Its input is six independent 0.05-amplitude sine waves at 220, 330, 440, 80,
550 and 660 Hz, in WAVE speaker order. It was generated for this project and
is provided under the project's MIT license; it contains no third-party media.

The committed ADTS fixture is consumed directly by the system AAC adapter tests,
including flush/drain and repeated seek/reset. Tests require no external encoder.

`ac3-surround.ac3` and `eac3-surround.eac3` contain four 48-kHz, 384-kbps,
5.1 syncframes encoded from the same project-owned six-channel tone.
Both system audio adapters exercise these bytes; when Dolby is not installed,
the tests verify the explicit unavailable-codec error instead of a fallback.
The matching `*-surround.mkv` files wrap these streams for real component-graph
integration tests on Windows and on both macOS architectures.

`av-subtitles.mkv` remuxes the existing synthetic `av.mkv` with two short SRT
subtitle cues. It exercises filtering of non-audio/video packet tracks during
playback, EOF and backward seeks; all media is the project's own synthetic corpus.
