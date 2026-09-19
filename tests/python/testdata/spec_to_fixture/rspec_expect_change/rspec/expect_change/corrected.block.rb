# nitrocop-config: EnforcedStyle: block
it do
  expect { run }.to change { User.count }.by(1)
end

it do
  expect { run }.to change { User::Token::Auth.count }.by(1)
end

it do
  expect { run }.to change { user.count }
end

it do
  expect { run }.to change { user.status }
end

it do
  expect { paint_users! }.to change { users.green.count }.by(1)
end

it do
  expect { run }.to change { ::User.count }
end

it do
  expect { paint_users! }.to change { @food.taste }.to(:sour)
end

it do
  expect { paint_users! }.to change { $token.value }.to(nil)
end

it do
  expect(run).to change { User.count }.by(1)
end
