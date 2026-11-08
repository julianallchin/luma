# Screenshots

The website's and README's screenshots: the real app, captured in the pixel
harness at retina scale, each window composed over a backdrop with rounded
corners, a soft shadow and a halftone fade into fog. The images land in
`www/public/screenshots/`.

```sh
scripts/screenshots/capture                         # the staged showcase library
scripts/screenshots/capture --library DIR           # a copy of a real library
scripts/screenshots/capture --library DIR SHOTS_DIR # raw captures kept there
```

| File | What it is for |
|---|---|
| `capture` | Runs one of the two capture scripts, then `composite.py`. |
| `showcase.test.js` | Captures from a staged library made of made-up names: the fixture definitions, cover art and audio in `harness/showcase/`. Anyone can run it. |
| `library.test.js` | Captures from a copy of a real library, named by `--library`. The venues, songs and scores it opens are constants at its top. |
| `composite.py` | Composes each capture over the backdrop: `backdrop.jpg` here when it exists, else the stage capture. |
| `backdrop.jpg` | The backdrop photo; see below. |
| `make_assets.py` | Writes the showcase library's art and audio. Run it only to change them. |

## A real library

`--library DIR` takes a directory holding a **copy** of `luma.db` with its
`-wal` and `-shm` files. Never point it at the live library in
`~/Library/Application Support/com.luma.luma/`: opening a library migrates it.
The fixture (`library` in `gpui/crates/agent/src/fixture.rs`) copies only the
database into a directory of its own for each shot, deletes every stored API
key, turns DMX and audio output off and signs the copy in with a session it
makes for the account that owns the rows. It never reads the library's own
`state.db`, and the harness keeps the app off the network. The rows go on
reading their audio and art where they name them.

Nothing from a real library is committed: only the composed images.

## The backdrop

`backdrop.jpg` is "laser show" by Artem Bryzgalov,
https://unsplash.com/photos/r4qdM8iD5WY, under the
[Unsplash License](https://unsplash.com/license): free for commercial use, no
attribution required (given here anyway). It is cropped to the beams and
resized to 2400 pixels wide; the same credit is in the file's JPEG comment.
A replacement must carry a licence as free, and no identifiable faces.
