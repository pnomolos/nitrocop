specify do
  expect { result }.to change { obj.foo }.from(1).to(2)
  change { obj.bar }.from(3).to(4)
  ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ RSpec/DiscardedMatcher: The result of `change` is not used. Did you mean to chain it with `.and`?
end

specify do
  expect { result }.to change(Foo, :bar).by(1)
  change(Foo, :baz).by(2)
  ^^^^^^^^^^^^^^^^^^^^^^ RSpec/DiscardedMatcher: The result of `change` is not used. Did you mean to chain it with `.and`?
end

it 'sets up message expectations' do
  expect(foo).to receive(:bar)
  receive(:baz).and_return(1)
  ^^^^^^^^^^^^^^^^^^^^^^^^^^^ RSpec/DiscardedMatcher: The result of `receive` is not used. Did you mean to chain it with `.and`?
  receive_message_chain(:baz, :qux)
  ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ RSpec/DiscardedMatcher: The result of `receive_message_chain` is not used. Did you mean to chain it with `.and`?
end

example 'captures output' do
  expect { result }.to output('foo').to_stdout
  output('bar').to_stderr
  ^^^^^^^^^^^^^^^^^^^^^^ RSpec/DiscardedMatcher: The result of `output` is not used. Did you mean to chain it with `.and`?
end

specify do
  expect(foo).to have_received(:bar)
  have_received(:baz)
  ^^^^^^^^^^^^^^^^^^ RSpec/DiscardedMatcher: The result of `have_received` is not used. Did you mean to chain it with `.and`?
end

specify do
  case condition
  when :update then expect { result }.to change { obj.bar }.from(3).to(4)
  else change { obj.baz }.from(5).to(6)
       ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ RSpec/DiscardedMatcher: The result of `change` is not used. Did you mean to chain it with `.and`?
  end
end

specify do
  def expect_action
    expect { action!(performer: performer) }
  end

  expect_action
    .to change { obj.foo }.from(1).to(2)
        change { obj.bar }.from(3).to(4)
        ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ RSpec/DiscardedMatcher: The result of `change` is not used. Did you mean to chain it with `.and`?
end
