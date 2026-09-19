# nitrocop-config: Strict: true
shared_context 'foo' do
^^^^^^^^^^^^^^^^^^^^ RSpec/SharedContext: Use `shared_examples` when you define examples.
  it 'performs actions' do
  end
end

shared_context 'bar' do
^^^^^^^^^^^^^^^^^^^^ RSpec/SharedContext: Use `shared_examples` when you define examples.
  let(:foo) { :bar }

  it 'performs actions' do
  end
end

shared_context 'baz' do
^^^^^^^^^^^^^^^^^^^^ RSpec/SharedContext: Use `shared_examples` when you define examples.
  let(:foo) { :bar }

  include_examples 'baz'
end
