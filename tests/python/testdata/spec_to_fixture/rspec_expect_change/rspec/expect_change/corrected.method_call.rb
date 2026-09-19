# nitrocop-config: EnforcedStyle: method_call
it do
  expect { run }.to change(User, :count).by(1)
end

it do
  expect(run).to change(User, :count).by(1)
end

it do
  expect { run }.to change(User::Token::Auth, :count).by(1)
end

it do
  expect { run }.to change(::User, :count).by(1)
end

it do
  expect { run }.to change(user, :name).to('Jack')
end
