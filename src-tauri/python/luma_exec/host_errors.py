"""Shared exceptions for the Python↔host capability boundary."""


class LumaHostCallError(RuntimeError):
    """A structured rejection from a host-side capability handler."""

    def __init__(self, code: str, message: str):
        self.code = code
        super().__init__(message)
