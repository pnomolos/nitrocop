# nitrocop-config: EnforcedStyle: separator, separator, always_ignore
# Corpus regression (antiwork/gumroad app/models/*_bank_account.rb, Shopify/ruby-lsp
# test/fixtures/hash_literal_omitted_values.rb): RuboCop's `Pair#value` for a Ruby 3.1
# value-omission pair (`foo:`) is a synthesized node whose location equals the key's
# own location -- there is no separate value token. When that omission pair is used as
# the `separator`-style reference (`first_pair`), any later pair with an explicit value
# almost never lands on that synthesized column, so RuboCop still flags it even though
# the *keys* happen to line up. `bank_account_type:` below is itself an omission pair,
# so its own value delta is forced to zero regardless (unaligned key still flags it).
hash = {
  routing_number:,
  account_number: value,
  ^^^^^^^^^^^^^^^^^^^^^^ Layout/HashAlignment: Align the separators of a hash literal if they span more than one line.
  bank_account_type:
  ^^^^^^^^^^^^^^^^^^ Layout/HashAlignment: Align the separators of a hash literal if they span more than one line.
}
