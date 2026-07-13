import sys
from .parser import parse_expression


def main():
    if len(sys.argv) < 2:
        print("Usage: python -m calc <expression>")
        print("Example: python -m calc '2 + 3'")
        sys.exit(1)

    expr = " ".join(sys.argv[1:])
    try:
        result = parse_expression(expr)
        print(f"{expr} = {result}")
    except ValueError as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
