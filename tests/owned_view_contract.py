"""Check shared input expectations; runtime bindings consume the same fixtures.

Run with uv run python tests/owned_view_contract.py. This oracle defines target
input semantics; it does not claim that the current binding APIs implement them.
"""
import json
import math
from pathlib import Path


def integer(token):
    if token == "-0":
        return -0.0
    value = int(token)
    if not -(1 << 63) <= value < (1 << 63):
        raise ValueError("integer outside i64")
    return value


def reject_constant(token):
    raise ValueError(token)


def check_tree(value, depth=0):
    if isinstance(value, (dict, list)):
        if depth >= 32:
            raise ValueError("container depth limit")
        entries = value.values() if isinstance(value, dict) else value
        if isinstance(value, dict):
            for key in value:
                if "\0" in key:
                    raise ValueError("NUL key")
                key.encode("utf-8")
        for child in entries:
            check_tree(child, depth + 1)
    elif isinstance(value, str):
        value.encode("utf-8")
    elif isinstance(value, float) and not math.isfinite(value):
        raise ValueError("non-finite float")


def describe(value):
    if value is None:
        return {"kind": "null"}
    if isinstance(value, bool):
        return {"kind": "bool", "value": value}
    if isinstance(value, int):
        return {"kind": "i64", "decimal": str(value),
                "javascript": "number" if abs(value) <= (1 << 53) - 1 else "bigint"}
    if isinstance(value, float):
        return {"kind": "f64", "hex": value.hex()}
    if isinstance(value, str):
        return {"kind": "string", "value": value}
    return {"kind": "object" if isinstance(value, dict) else "array", "length": len(value)}


def run(case):
    source = case.get("json")
    if source is None:
        source = '{"nested":' + '[' * case["nested_arrays"] + '0' + ']' * case["nested_arrays"] + '}'
    data = json.loads(source, parse_int=integer, parse_constant=reject_constant)
    if not isinstance(data, dict):
        raise ValueError("data must be object")
    check_tree({"meta": {"tags": []}, "data": data})
    for key, expected in case.get("fields", {}).items():
        actual = {"kind": "missing"} if key not in data else describe(data[key])
        assert actual == expected, (case["id"], key, actual, expected)
    return data


def main():
    cases = json.loads((Path(__file__).parent / "fixtures" / "owned_views.json").read_text())
    for case in cases:
        try:
            run(case)
        except (ValueError, UnicodeError):
            assert case["outcome"] == "reject", case["id"]
        else:
            assert case["outcome"] == "accept", case["id"]
    print(f"Checked {len(cases)} owned-view input cases")


if __name__ == "__main__":
    main()
