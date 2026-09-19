# nitrocop-config: EnforcedStyle: method_call
it do
  expect { run }.to change { User.sum(:points) }
end

it do
  Record.change { User.count }
end

it do
  expect { run }.to change(User, :count).by(1)
end

it do
  expect { run }.to change { user.reload.name }
end

it do
  expect { run }.to change { results }.to([])
end
