x = 1 +
        2
        ^ Layout/MultilineOperationIndentation: Align the operands of an expression in an assignment spanning multiple lines.
z = 5 +
      6
      ^ Layout/MultilineOperationIndentation: Align the operands of an expression in an assignment spanning multiple lines.
w = a &&
         b
         ^^^^ Layout/MultilineOperationIndentation: Align the operands of an expression in an assignment spanning multiple lines.

# Chained || with same-indent continuations (most common FN pattern)
def skip?
  a ||
  b ||
  ^ Layout/MultilineOperationIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.
  c
  ^ Layout/MultilineOperationIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.
end

# Multiline && in if condition - misaligned
if a &&
  b
  ^ Layout/MultilineOperationIndentation: Align the operands of a condition in an `if` statement spanning multiple lines.
  do_something
end

# FN: Assignment with chained + continuation at wrong indent
result = foo("h3") +
  foo("p1") +
  ^ Layout/MultilineOperationIndentation: Align the operands of an expression in an assignment spanning multiple lines.
  foo("p2")
  ^ Layout/MultilineOperationIndentation: Align the operands of an expression in an assignment spanning multiple lines.

# Same-indent chained + in assignment with wrong indent
result2 = "hello".capitalize +
  "world" +
  ^ Layout/MultilineOperationIndentation: Align the operands of an expression in an assignment spanning multiple lines.
  "foo"
  ^ Layout/MultilineOperationIndentation: Align the operands of an expression in an assignment spanning multiple lines.

# Same-column chained + in a method body is still an offense
def lyrics
  "hello".capitalize +
  "world" +
  ^ Layout/MultilineOperationIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.
  "foo" +
  ^ Layout/MultilineOperationIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.
  "bar"
  ^ Layout/MultilineOperationIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.
end

# Operator calls used as method arguments must align in `aligned` style.
puts a, 1 +
  2
  ^ Layout/MultilineOperationIndentation: Align the operands of an expression spanning multiple lines.

it "should convert " +
  "a to " +
  ^^^^^^^ Layout/MultilineOperationIndentation: Align the operands of an expression spanning multiple lines.
  "b" do
  ^^^ Layout/MultilineOperationIndentation: Align the operands of an expression spanning multiple lines.
end

# Boolean operation inside a block body with over-indented operands.
values = fields.map { |attrs| attrs.value }
               .reject { |v| v.empty? ||
                             v == "updatedns" ||
                             ^ Layout/MultilineOperationIndentation: Use 2 (not 14) spaces for indenting an expression spanning multiple lines.
                             v == "Submit"
                             ^ Layout/MultilineOperationIndentation: Use 2 (not 14) spaces for indenting an expression spanning multiple lines.
                       }

# Boolean chains passed as keyword arguments in method calls align to argument start.
it "reports errors", if: RUBY_VERSION < "2.6" ||
  PlatformHelpers.truffleruby? || PlatformHelpers.jruby? &&
  ^ Layout/MultilineOperationIndentation: Align the operands of an expression spanning multiple lines.
    Gem::Version.new(RUBY_ENGINE_VERSION) >= "9.3.7.0" do
    ^ Layout/MultilineOperationIndentation: Align the operands of an expression spanning multiple lines.
end

# Operator inside a case expression that is an assignment RHS still uses
# assignment alignment in aligned style.
def get_date_filter(operator)
  filter = case operator
    when OPERATOR_TODAY
      "BETWEEN" +
        "AND"
        ^ Layout/MultilineOperationIndentation: Align the operands of an expression in an assignment spanning multiple lines.
    end
end

# Unrelated parentheses earlier/later in the file must not suppress the cop.
# This mirrors full-file corpus FNs where a snippet detects but the full file
# was skipped by the old source scanner.
# option docs (
def initialize_client
  if proxy_missing
    raise ArgumentError, "Proxy IP and port must both be specified or" +
          " both left nil"
          ^ Layout/MultilineOperationIndentation: Align the operands of an expression spanning multiple lines.
  end
end
# option docs )

# `indentation` is `source_line =~ /\S/`, so a leading tab counts as one
# column: the expected indentation here is 1 (tab) + 2.
def tab_indented
	value ||
	other
	^ Layout/MultilineOperationIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.
end

# `keyword_message_tail` uses `loc.keyword.source`, so an `elsif` names itself
# (and takes the `a` article, since only `i`/`u` keywords take `an`).
def elsif_condition(a, b)
  if a
    1
  elsif a &&
  b
  ^ Layout/MultilineOperationIndentation: Align the operands of a condition in a `elsif` statement spanning multiple lines.
    2
  end
end

# `numblock` is a distinct parser type that `disqualified_rhs?`'s
# `block_type?` test does not match, so the outer assignment is still found.
def numbered_block_parameter(list)
  x = list.map { _1 +
                   2 }
                   ^ Layout/MultilineOperationIndentation: Align the operands of an expression in an assignment spanning multiple lines.
end

# `begin ... end while cond` is `while_post`, which is in neither
# `KEYWORD_ANCESTOR_TYPES` nor `UNALIGNED_RHS_TYPES` — so this reports as a
# plain expression, not as a condition in a `while` statement.
def post_condition_loop(a, b)
  begin
    a
  end while a ||
  b
  ^ Layout/MultilineOperationIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.
end

# `used_indentation` is `rhs.column - indentation(lhs)` and may be negative.
def negative_used_indentation(a, b)
      a ||
  b
  ^ Layout/MultilineOperationIndentation: Use 2 (not -4) spaces for indenting an expression spanning multiple lines.
end
