# Stand-in interpreters

Each of these pretends to be `python3` and misbehaves in one specific way, so
the plugin bridge can be tested against a plugin that hangs, crashes, returns
nonsense, or says nothing at all.

They are committed rather than written by the tests. Writing an executable and
then exec'ing it inside a multi-threaded process races with `fork`: another
thread's copy of the write descriptor can still be open when `execve` runs, and
the kernel answers `ETXTBSY`. Committing them removes the write entirely.
