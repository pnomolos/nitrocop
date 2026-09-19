case value
in x
  handle_other
in Integer
^^^^^^^^^^ Lint/UnreachablePatternBranch: Unreachable `in` pattern branch detected.
  handle_integer
end

case value
in Integer
  handle_integer
in _
  handle_other
else
^^^^ Lint/UnreachablePatternBranch: Unreachable `else` branch detected.
  handle_else
end

case value
in _ => y
  handle_other
in String
^^^^^^^^^ Lint/UnreachablePatternBranch: Unreachable `in` pattern branch detected.
  handle_string
in Symbol
^^^^^^^^^ Lint/UnreachablePatternBranch: Unreachable `in` pattern branch detected.
  handle_symbol
end

case value
in _ | Integer
  handle_other
in String
^^^^^^^^^ Lint/UnreachablePatternBranch: Unreachable `in` pattern branch detected.
  handle_string
end

case value
in Integer | _
  handle_other
in String
^^^^^^^^^ Lint/UnreachablePatternBranch: Unreachable `in` pattern branch detected.
  handle_string
end

case value
in (_ | Integer) => y
  handle_other
in String
^^^^^^^^^ Lint/UnreachablePatternBranch: Unreachable `in` pattern branch detected.
  handle_string
end

case value
in x if x.positive?
  handle_positive
in y
  handle_other
in Integer
^^^^^^^^^^ Lint/UnreachablePatternBranch: Unreachable `in` pattern branch detected.
  handle_integer
else
^^^^ Lint/UnreachablePatternBranch: Unreachable `else` branch detected.
  handle_else
end

case value
in (z)
  handle_any
else
^^^^ Lint/UnreachablePatternBranch: Unreachable `else` branch detected.
  handle_else
end
