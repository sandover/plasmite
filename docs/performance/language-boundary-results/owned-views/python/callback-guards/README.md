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
handle. Median small-send time changes from 5.026 to5.102 microseconds; wide
changes from 19.805 to20.398, and 64 KiB string changes from 14.569 to14.208.
These short samples establish the scale of overhead, not a performance win.
The raw file records interpreter and library hashes. The
[exact measured runner](measured-runner.py) predates formatting of the maintained
benchmark. The existing producer source archive supplies the old facade;
the corrected facade lives in this commit. No runtime C library changed.
