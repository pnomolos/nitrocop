array.select
array.select { |x| x.even? }
array.select { |x| y.between?(1, 10) }
array.select { |x, y| x.between?(1, 10) }
array.select { |x| (1..10).cover?(y) }
array.select { |x| (1..10).member?(x) }
array.select { |x| x.between?(1) }
array.select { |x| RANGE.cover?(x) }
array.select { _2.between?(1, 10) }
array.select { |x| !x.even? }
array.select do |x|
  next if x.even?
  x.between?(1, 10)
end
{}.select { |x| x.between?(1, 10) }
{ foo: :bar }.select { |x| x.between?(1, 10) }
Hash.new.select { |x| x.between?(1, 10) }
Hash.new(:default).select { |x| x.between?(1, 10) }
Hash.new { |h, k| h[k] = [] }.select { |x| x.between?(1, 10) }
Hash[pairs].select { |x| x.between?(1, 10) }
to_h.select { |x| x.between?(1, 10) }
foo.to_h.select { |x| x.between?(1, 10) }
foo.to_hash.select { |x| x.between?(1, 10) }
ENV.select { |x| x.between?(1, 10) }
::ENV.select { |x| x.between?(1, 10) }
array.map { |x| x.between?(1, 10) }
array.select(&:even?)
