"""Shared exceptions for the Python↔host capability boundary."""


class LumaHostCallError(RuntimeError):
    """A structured rejection from a host-side capability handler."""

    def __init__(self, code: str, message: str):
        self.code = code
        super().__init__(message)


class VenueRefused(RuntimeError):
    """A `luma.venue` verb the resolver would not accept.

    The design's two hard errors, and nothing else: a socket pair the catalog
    forbids, and an extend longer than the ray-measured gap. Everything else a
    verb has to say arrives as a warning on the `Placement` it returns, so
    `except luma.VenueRefused` catches exactly the calls that changed nothing.

    The message is the resolver's own, verbatim — it names the pair or the gap,
    and it is the fix.
    """

    def __init__(self, reason: str):
        self.reason = reason
        super().__init__(reason)
