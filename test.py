import asyncio
import inspect

from veight import Isolates, Value

CONTENT = """
(async () => {
    return await hello("money");
})()
"""


async def hello(arg: Value):
    return arg.to_str()


async def main():
    isolates = Isolates()
    isolate = await isolates.create_isolate()

    isolate.add_function("hello", hello)

    res = await isolate.run(CONTENT)

    if inspect.isawaitable(res):
        print((await res).to_undefined())
    else:
        print(res.to_str())


asyncio.run(main())
