# nitrocop-config: CustomMatcherMethods: [custom_matcher]
specify do
  expect(foo).to \
    receive(:bar)
    custom_matcher(:foo)
    ^^^^^^^^^^^^^^^^^^^^ RSpec/DiscardedMatcher: The result of `custom_matcher` is not used. Did you mean to chain it with `.and`?
end

specify do
  expect(foo).to custom_matcher(:bar)
end
