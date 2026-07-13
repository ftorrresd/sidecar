import re
from .operations import add, subtract, multiply, divide, power


OPS = {
    "+": add,
    "-": subtract,
    "*": multiply,
    "/": divide,
    "^": power,
}


def parse_expression(expr: str) -> float:
    tokens = re.findall(r"(\d+\.?\d*|[+\-*/^])", expr.replace(" ", ""))
    if len(tokens) < 3:
        raise ValueError(f"Invalid expression: {expr}")

    a = float(tokens[0])
    op = tokens[1]
    b = float(tokens[2])

    if op not in OPS:
        raise ValueError(f"Unknown operator: {op}")

    return OPS[op](a, b)
