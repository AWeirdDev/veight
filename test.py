import asyncio

from veight import Isolate, Isolates, Value

CONTENT = """
hello()
"""


async def hello(isolate: Isolate, arg: Value) -> Value:
    return Value.create_number(isolate, 100)


async def main():
    isolates = Isolates()
    isolate = await isolates.create_isolate()
    isolate.add_function("hello", hello)
    res = await isolate.run(CONTENT)
    print((await res).is_bigint())
    isolate.close()


asyncio.run(main())
