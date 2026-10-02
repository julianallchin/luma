"""Shared exceptions for the Python↔host capability boundary."""


class LumaHostCallError(RuntimeError):
    """A structured rejection from a host-side capability handler."""

    def __init__(self, code: str, message: str):
        self.code = code
        super().__init__(message)


class VenueRefused(RuntimeError):
    """A `luma.venue` build verb the room would not accept. Nothing was written.

    Raised for what the room itself refuses: a socket pair the catalog forbids,
    an extend longer than the measured gap, a turn no joint makes, a collision,
    an end that is not open, and a piece name that is neither in the catalog
    nor in the fixture library. The message says what to change.

    Other failures are different errors. A bad argument (wrong type, not
    finite, out of range) raises `LumaHostCallError`, `ValueError` or
    `TypeError`. An unknown node id raises `LumaHostCallError` with code
    `not_found`. A row that does not fit in `distribute` is not an error: it
    returns a `Distribution` with `ok` false.
    """

    def __init__(self, reason: str):
        self.reason = reason
        super().__init__(reason)
