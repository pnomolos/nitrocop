array.select

array.select(&:even?)

array.select { |x| x.even? }

array.select do |x|
  next if x.nil?
  x.is_a?(Foo)
end

obj.select { |x, y| y.is_a?(Foo) }

array.select { |x| y.is_a?(Foo) }

array.select { _1.is_a?(_2) }

{}.select { |x| x.is_a?(Foo) }

{ foo: :bar }.select { |x| x.is_a?(Foo) }

Hash.new.select { |x| x.is_a?(Foo) }

Hash.new(:default).select { |x| x.is_a?(Foo) }

Hash.new { |hash, key| :default }.select { |x| x.is_a?(Foo) }

Hash[h].select { |x| x.is_a?(Foo) }

Hash[:foo, 0, :bar, 1].select { |x| x.is_a?(Foo) }

to_h.select { |x| x.is_a?(Foo) }

foo.to_h.select { |x| x.is_a?(Foo) }

to_hash.select { |x| x.is_a?(Foo) }

foo.to_hash.select { |x| x.is_a?(Foo) }

ENV.select { |x| x.is_a?(Foo) }

::ENV.select { |x| x.is_a?(Foo) }

array.map { |x| x.is_a?(Foo) }
