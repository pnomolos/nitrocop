# nitrocop-config: NegatedMatcher: not_change
expect { run }.to change(Foo, :bar).and not_change(Foo, :baz)
