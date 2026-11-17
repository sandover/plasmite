# README visuals

The README uses three visuals: the local pool model, a terminal example, and
the live browser map. Each has a different job. The diagram shows who shares
the file; the terminal shows a reader arriving after a writer exits; the map
shows how to inspect retained messages and live traffic.

## Local IPC recording

`ipc/local-ipc.gif` records the 2.0 command syntax in an actual shell.
The writer finishes before the reader starts. The reader exits after the
first retained message.

`ipc/local-ipc.cast` contains the terminal's original bytes and timings, plus
the CLI version and binary SHA-256. `ipc/local-ipc.png` is the final still.
The recording measures 1220 × 370 pixels. The cast identifies the candidate
version and exact CLI binary.
The renderer uses the terminal bytes, without inventing or replacing output.

After the 2.0 release, install Plasmite 2.0.0 on macOS and run from the repo root.

```bash
uv run scripts/record_readme_demo.py --font /System/Library/Fonts/Menlo.ttc
```

The script requires a monospace TTF/TTC font; pass another font path on Linux.
It creates its own temporary pool directory and removes it when finished.
It checks the installed version and reads the retained message back before
writing the assets. For a 2.0 candidate, add `--candidate` and put that build on `PATH`.

## Pool diagram

`ipc/pool-model.svg` is an editable diagram of two writers and two independent
readers sharing one local file. The JSON boxes stand for retained messages;
they do not describe the file's byte layout or capacity overhead. Reading
does not remove a message, and new writes overwrite old history when full.

## Browser map

`ui/pool-map.gif` shows sample events in four pools, message previews, and
hover highlights on the spiral. `ui/pool-map.png` is frame 120 of that existing
recording, for readers who prefer a still image. The GIF remains unchanged.

The older `ui-pool-list.png` and `ui-pool-follow.png` show earlier UI layouts.
They are not featured in the README. Capture the current released UI before
using those views in new documentation.
