# nitrocop-config: EnforcedStyle: block
it do
  expect { run }.to change(User, :count).by(1)
                    ^^^^^^^^^^^^^^^^^^^^ RSpec/ExpectChange: Prefer `change { User.count }`.
end

it do
  expect { run }.to change(User::Token::Auth, :count).by(1)
                    ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ RSpec/ExpectChange: Prefer `change { User::Token::Auth.count }`.
end

it do
  expect { run }.to change(user, :count)
                    ^^^^^^^^^^^^^^^^^^^^ RSpec/ExpectChange: Prefer `change { user.count }`.
end

it do
  expect { run }.to change(user, 'status')
                    ^^^^^^^^^^^^^^^^^^^^^^ RSpec/ExpectChange: Prefer `change { user.status }`.
end

it do
  expect { paint_users! }.to change(users.green, :count).by(1)
                             ^^^^^^^^^^^^^^^^^^^^^^^^^^^ RSpec/ExpectChange: Prefer `change { users.green.count }`.
end

it do
  expect { run }.to change(::User, :count)
                    ^^^^^^^^^^^^^^^^^^^^^^ RSpec/ExpectChange: Prefer `change { ::User.count }`.
end

it do
  expect { paint_users! }.to change(@food, :taste).to(:sour)
                             ^^^^^^^^^^^^^^^^^^^^^ RSpec/ExpectChange: Prefer `change { @food.taste }`.
end

it do
  expect { paint_users! }.to change($token, :value).to(nil)
                             ^^^^^^^^^^^^^^^^^^^^^^ RSpec/ExpectChange: Prefer `change { $token.value }`.
end

it do
  expect(run).to change(User, :count).by(1)
                 ^^^^^^^^^^^^^^^^^^^^ RSpec/ExpectChange: Prefer `change { User.count }`.
end
