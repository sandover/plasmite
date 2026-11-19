# Python callback ordering

Python callbacks can close a pool while the same thread holds its reentrant
lock. Preparing tags, durability, sequence numbers or encoded references after
capturing its native pointer could then pass a freed pointer into C. The
bindings now finish those conversions, check that the handle remains open,
and borrow its current pointer immediately before the native call.

Bytes subclasses also need their physical buffer extent. The bindings strip
the subclass with the base bytes slice operation before using its length.
An overridden `__len__` cannot enlarge the native read beyond the buffer.

The focused tests mock the final C boundary when a callback closes a handle;
they fail safely if the ordering regresses. The full Python suite passes 80
tests with one expected skip. Native and fallback view tests both pass.

[Raw measurements](raw.json) alternate the old and corrected append-view
ordering seven times with literal builtins, where no callback can close a
handle. Median send times in microseconds were 5.026 to 5.102 for the small
fixture, 19.805 to 20.398 for the wide fixture, and 14.569 to 14.208 for the
64 KiB string.
These short samples establish the scale of overhead, not a performance win.
The raw file records interpreter and library hashes. The
[exact measured runner](measured-runner.py) predates formatting of the maintained
benchmark. The producer source archive supplies the old facade. Commit
`646b9b4` contains the corrected facade, and the comparison uses the same C
library on both sides.
