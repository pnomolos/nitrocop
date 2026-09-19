# nitrocop-config: EnforcedStyle: separator, separator, always_ignore
# Corpus regression (DamirSvrtan/fasterer lib/fasterer/offense.rb): the right-align
# key delta needed here lands exactly on the boundary where the corrector's backward
# value-removal reaches only into the *separator* token (`:`), not into the key's own
# range. RuboCop does not crash in this shape, unlike the emits_before_clobber /
# first_value_newline_equal_key cases below, where the delta is one character (or
# more) larger and the removal reaches past the separator into the key itself.
boundary_absorbed = {
  "longkey":
    first,
  "a":
  ^^^^ Layout/HashAlignment: Align the separators of a hash literal if they span more than one line.
    second
}
