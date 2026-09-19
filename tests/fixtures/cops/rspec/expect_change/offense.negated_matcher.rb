# nitrocop-config: NegatedMatcher: not_change
expect { run }.to not_change { User.count }
                  ^^^^^^^^^^^^^^^^^^^^^^^^^ RSpec/ExpectChange: Prefer `not_change(User, :count)`.
