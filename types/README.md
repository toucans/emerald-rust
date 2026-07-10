# types — the seam

The only thing the game and the backends share: plain data (`Quad`, `Frame`, `Rect`,
`Rgba`, `Flip`, `Input`, `TextureId`) plus the two port traits (`Backend`,
`AudioSink`). Zero dependencies, no behavior.

Rules (see the root [README](../README.md), "The one decision"):

- No backend crate's vocabulary may appear here, ever.
- `Quad` and the traits do **not** grow as the game grows richer — only the code that
  produces the data does. A new `Quad` field needs the pret data to force it.
