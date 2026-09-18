"""CPython counterparts for benches/language_compare.rs.

Each source is compiled once. Timings include execution and creation of its fresh
globals dictionary, but exclude parsing/bytecode compilation.
"""

from __future__ import annotations

import gc
import json
import statistics
import time


CASES = {
    "arith_loop_200k": """
total = 0
i = 0
for _ in range(200000):
    total += i
    i += 1
result = total
""",
    "function_calls_100k": """
def step(x):
    return x + 1
value = 0
for _ in range(100000):
    value = step(value)
result = value
""",
    "recursive_fib_25": """
def fib(n):
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)
result = fib(25)
""",
    "recursive_fib_30": """
def fib(n):
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)
result = fib(30)
""",
    "nested_loop_500x500": """
total = 0
for i in range(500):
    for j in range(500):
        total += i + j
result = total
""",
    "list_map_filter_20k": """
values = range(20000)
mapped = [x * 3 for x in values if x % 2 == 0]
result = len(mapped)
""",
    "list_build_50k": """
values = [x * x for x in range(50000)]
result = len(values)
""",
    "dict_counter_20k": """
counts = {}
for x in range(20000):
    key = str(x % 100)
    counts[key] = counts.get(key, 0) + 1
result = counts["42"]
""",
    "dict_lookup_100k": """
values = {str(x): x for x in range(1000)}
total = 0
for x in range(100000):
    total += values[str(x % 1000)]
result = total
""",
    "string_join_10k": """
parts = [str(x) for x in range(10000)]
result = len(",".join(parts))
""",
    "text_conversion_20k": """
total = 0
for x in range(20000):
    total += len(str(x))
result = total
""",
    "json_roundtrip_2k": """
import json
source = list(range(2000))
encoded = json.dumps(source, separators=(",", ":"))
result = len(json.loads(encoded))
""",
    "exception_catch_10k": """
caught = 0
for _ in range(10000):
    try:
        raise ValueError("x")
    except ValueError:
        caught += 1
result = caught
""",
}

EXPECTED = {
    "arith_loop_200k": 19_999_900_000,
    "function_calls_100k": 100_000,
    "recursive_fib_25": 75_025,
    "recursive_fib_30": 832_040,
    "nested_loop_500x500": 124_750_000,
    "list_map_filter_20k": 10_000,
    "list_build_50k": 50_000,
    "dict_counter_20k": 200,
    "dict_lookup_100k": 49_950_000,
    "string_join_10k": 48_889,
    "text_conversion_20k": 88_890,
    "json_roundtrip_2k": 2_000,
    "exception_catch_10k": 10_000,
}


def run_once(code: object) -> object:
    namespace: dict[str, object] = {}
    exec(code, namespace)
    return namespace["result"]


def measure(operation: object, target_seconds: float = 0.25) -> tuple[float, int]:
    loops = 1
    while True:
        start = time.perf_counter()
        for _ in range(loops):
            operation()  # type: ignore[operator]
        if time.perf_counter() - start >= target_seconds:
            break
        loops *= 2

    samples: list[float] = []
    gc.disable()
    try:
        for _ in range(9):
            start = time.perf_counter_ns()
            for _ in range(loops):
                operation()  # type: ignore[operator]
            samples.append((time.perf_counter_ns() - start) / loops)
    finally:
        gc.enable()
    return statistics.median(samples), loops


def main() -> None:
    results: dict[str, dict[str, float | int]] = {}
    for name, source in CASES.items():
        code = compile(source, f"<{name}>", "exec")
        assert run_once(code) == EXPECTED[name]

        median_ns, loops = measure(lambda: run_once(code))
        results[name] = {"median_ns": median_ns, "loops": loops}
        print(f"{name:24} {median_ns / 1_000_000:10.3f} ms  ({loops} loops/sample)")

    lifecycle_source = """
def square(x):
    return x * x
values = [square(x) for x in range(1000)]
result = len(values)
"""
    lifecycle_code = compile(lifecycle_source, "<lifecycle>", "exec")
    compile_ns, compile_loops = measure(
        lambda: compile(lifecycle_source, "<lifecycle>", "exec")
    )
    run_ns, run_loops = measure(
        lambda: run_once(compile(lifecycle_source, "<lifecycle>", "exec"))
    )
    print("\nLifecycle (CPython)")
    print(f"{'compile_medium':24} {compile_ns / 1_000_000:10.3f} ms  ({compile_loops} loops/sample)")
    print(f"{'fresh_compile_run':24} {run_ns / 1_000_000:10.3f} ms  ({run_loops} loops/sample)")
    assert run_once(lifecycle_code) == 1000

    print("\nJSON")
    print(json.dumps(results, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
