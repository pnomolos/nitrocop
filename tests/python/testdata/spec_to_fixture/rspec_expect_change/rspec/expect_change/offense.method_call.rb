# nitrocop-config: EnforcedStyle: method_call
it do
  expect { run }.to change { User.count }.by(1)
                    ^^^^^^^^^^^^^^^^^^^^^ RSpec/ExpectChange: Prefer `change(User, :count)`.
end

it do
  expect(run).to change { User.count }.by(1)
                 ^^^^^^^^^^^^^^^^^^^^^ RSpec/ExpectChange: Prefer `change(User, :count)`.
end

it do
  expect { run }.to change { User::Token::Auth.count }.by(1)
                    ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ RSpec/ExpectChange: Prefer `change(User::Token::Auth, :count)`.
end

it do
  expect { run }.to change { ::User.count }.by(1)
                    ^^^^^^^^^^^^^^^^^^^^^^^ RSpec/ExpectChange: Prefer `change(::User, :count)`.
end

it do
  expect { run }.to change { user.name }.to('Jack')
                    ^^^^^^^^^^^^^^^^^^^^ RSpec/ExpectChange: Prefer `change(user, :name)`.
end
