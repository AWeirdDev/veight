# veight
high-level asynchronous v8 bindings for python (in attempts to be safe)

```python
import asyncio
from veight import Isolates, Value

SCRIPT = """
// some javascript code
(async () => {
    return await hello("world");
})()
"""


async def hello(arg: Value) -> str:
    return arg.to_str()


async def main():
    isolates = Isolates()
    isolate = await isolates.create_isolate()

    isolate.add_function("hello", hello)

    # run script
    res = await isolate.run(SCRIPT)
    print(res)


asyncio.run(main())
```

## why
why not
