x = 1
y = 2
z = 3
a = 4
b = 5
c = 6

# Renamed cop (Metrics/LineLength → Layout/LineLength) with an offense on the
# disabled line — the directive suppresses a real offense, so it is not redundant.
# rubocop:disable Metrics/LineLength
this_is_a_very_long_line_that_should_trigger_line_length_cop_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa = 1
# rubocop:enable Metrics/LineLength

# FP fix: malformed cop name (/BlockLength) — not a valid cop name, ignore silently
# rubocop:disable /BlockLength, Metrics/
x = 1
# rubocop:enable /BlockLength, Metrics/

# Bare cop name that really suppresses an offense — `LineLength` qualifies to
# `Layout/LineLength`, which fires on this line, so the directive is needed.
this_is_a_very_long_line_that_should_trigger_line_length_cop_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb = 1 # rubocop:disable LineLength

# Real department names stay conservative — never flagged.
# rubocop:disable Metrics
def long_method
  1
end
# rubocop:enable Metrics

# An `enable` closes the first range, so re-disabling afterwards opens a fresh
# range and is judged only on whether it suppressed an offense.
# rubocop:disable Style/SymbolProc
things.map { |t| t.foo }
# rubocop:enable Style/SymbolProc
plain = 1
# rubocop:disable Style/SymbolProc
others.map { |t| t.foo }
# rubocop:enable Style/SymbolProc
