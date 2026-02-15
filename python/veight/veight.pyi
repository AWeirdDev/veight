from __future__ import annotations

from typing import Any, Awaitable, Callable

class Isolates:
    """A frozen singleton for V8 Isolates.

    That is:
    ```python
    isolates0 = Isolates()
    isolates1 = Isolates()
    assert isolates0 is isolates1
    ```

    To create an isolates, see the asynchronous method `Isolates.create_isolate()`.
    """

    def __init__(self) -> None: ...
    async def create_isolate(self, *, no_python: bool = False) -> Isolate:
        """Create a new isolate, returning the isolate handle.

        Args:
            no_python (bool, optional): Whether to disable Python-side handling.
                You may save a few resources.
        """
    def isolate_session(self) -> IsolateSession:
        """Create an isolate session with an asynchronous context manager.

        When out of the `with` context, the isolate session gets closed.

        Python-side handling is enabled by default. (`no_python=False`)

        ```python
        async with isolates.isolate_session() as isolate:
            ...
        ```
        """

class Isolate:
    def add_function(
        self, name: str, f: Callable[[Isolate, Any], Awaitable[Value]]
    ): ...
    async def run(self, source: str) -> Any:
        """Run Javascript code.

        Args:
            source (str): Code.
        """

    def close(self):
        """Close this isolate."""

class IsolateSession:
    async def __aenter__(self) -> Isolate: ...
    async def __aexit__(self, *_) -> None: ...

class Value:
    """Represents a v8 value.

    Upon conversion, the value itself gets released,
    meaning you can only do conversion once.
    """
    @staticmethod
    def create_string(isolate: Isolate, data: str) -> Value: ...
    @staticmethod
    def create_number(isolate: Isolate, data: float) -> Value: ...
    @staticmethod
    def create_undefined(isolate: Isolate) -> Value: ...
    @staticmethod
    def create_null(isolate: Isolate) -> Value: ...

    # checks
    def is_int32(self) -> bool: ...
    def is_uint32(self) -> bool: ...
    def is_float64(self) -> bool: ...
    def is_bigint(self) -> bool: ...
    def is_boolean(self) -> bool: ...
    def is_string(self) -> bool: ...
    def is_undefined(self) -> bool: ...
    def is_null(self) -> bool: ...
    def is_null_or_undefined(self) -> bool: ...

    # conversions
    def to_none(self) -> None:
        """Converts to `None` if the value is null or undefined."""
    def to_undefined(self) -> undefined:
        """Converts to `undefined` if the value is null or undefined."""
    def to_str(self) -> str: ...
    def to_bool(self) -> bool: ...
    def to_bigint(self) -> int: ...
    def to_float(self) -> float: ...
    def to_uint(self) -> int: ...
    def to_int(self) -> int: ...
    def release(self) -> None:
        """Release (drop) the value, making it no longer convertable."""

class undefined:
    """A Python singleton representing Javascript 'undefined.'

    You should not compare different undefined values across
    isolates.
    """

class JsError(Exception):
    """Javascript error that extend from `Error`.

    Errors like `ReferenceError`, `SyntaxError`, and more can be caught.

    ```python
    try:
        await isolate.run(some_js)
    except JsError as err:
        print(err)
    ```
    """
