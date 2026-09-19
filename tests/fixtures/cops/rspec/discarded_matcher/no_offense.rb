specify do
  expect { result }.to change { obj.foo }.from(1).to(2)
end

specify do
  expect { result }.to \
    change { obj.foo }.from(1).to(2)
    .and change { obj.bar }.from(3).to(4)
end

specify do
  expect { result }.to receive_messages(foo: 1, bar: 2)
end

specify do
  allow(foo).to receive(:bar).and_return(1)
end

specify do
  output.rewind
  parsed = JSON.parse(output.read.chomp)
  expect(parsed['message']).to eq('error message')
end

specify do
  expect { result }.to change { obj.bar } if condition
end

specify do
  expect { result }.to change { obj.bar } unless condition
end

specify do
  change_temp =
    if condition
      change { obj.bar }.from(3).to(4)
    else
      change { obj.baz }.from(5).to(6)
    end

  expect { result }.to change_temp
end

specify do
  change_temp = case season
                when :summer then change { temp }.from(0).to(1)
                else change { temp }.from(0).to(2)
                end

  expect { result }.to change_temp
end

specify do
  expect { result }.to \
    change { obj.foo }.from(1).to(2) &
    change { obj.bar }.from(3).to(4)
end

specify do
  expect { result }.to change { obj.bar }.from(1).to(2)
  foo.change { obj.foo }
end

specify do
  foo.change { obj.foo }
end

expect { result }.to change { obj.bar }

expect { result }.to (change { obj.bar })

expect { result }.to \
  change { obj.foo }
  change { obj.bar }
