case value
in Integer
  handle_integer
in String
  handle_string
else
  handle_other
end

case value
in Integer
  handle_integer
in String
  handle_string
in x
  handle_other
end

case value
in x if x.positive?
  handle_positive
in Integer
  handle_integer
end

case value
in x unless x.nil?
  handle_not_nil
in Integer
  handle_integer
end

case value
in [*]
  handle_array
in Integer
  handle_integer
end

case value
in **rest
  handle_hash
in Integer
  handle_integer
end

case value
in Integer | String
  handle_int_or_string
in Symbol
  handle_symbol
end

case value
in Integer => y
  handle_integer
in String
  handle_string
end

case value
in 1
  handle_one
in 2
  handle_two
end

case value
in [*, 1, *]
  handle_contains_one
in Integer
  handle_integer
end

case value
in x
  handle_any
end

case value
when Integer
  handle_integer
when String
  handle_string
else
  handle_other
end
