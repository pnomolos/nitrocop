array.map(&:to_s).join(', ')
      ^^^ Style/MapJoin: Remove redundant `map(&:to_s)` before `join`.
array.collect(&:to_s).join(', ')
      ^^^^^^^ Style/MapJoin: Remove redundant `collect(&:to_s)` before `join`.
array.map(&:to_s).join
      ^^^ Style/MapJoin: Remove redundant `map(&:to_s)` before `join`.
array.map { |x| x.to_s }.join(', ')
      ^^^ Style/MapJoin: Remove redundant `map(&:to_s)` before `join`.
array.collect { |x| x.to_s }.join(', ')
      ^^^^^^^ Style/MapJoin: Remove redundant `collect(&:to_s)` before `join`.
array&.map(&:to_s)&.join(', ')
       ^^^ Style/MapJoin: Remove redundant `map(&:to_s)` before `join`.
map(&:to_s).join(', ')
^^^ Style/MapJoin: Remove redundant `map(&:to_s)` before `join`.
collect(&:to_s).join(', ')
^^^^^^^ Style/MapJoin: Remove redundant `collect(&:to_s)` before `join`.
map { |x| x.to_s }.join(', ')
^^^ Style/MapJoin: Remove redundant `map(&:to_s)` before `join`.
array.map { _1.to_s }.join(', ')
      ^^^ Style/MapJoin: Remove redundant `map(&:to_s)` before `join`.
array.map { it.to_s }.join(', ')
      ^^^ Style/MapJoin: Remove redundant `map(&:to_s)` before `join`.
array
  .map(&:to_s)
   ^^^ Style/MapJoin: Remove redundant `map(&:to_s)` before `join`.
  .join(', ')
array
  .map { |x| x.to_s }
   ^^^ Style/MapJoin: Remove redundant `map(&:to_s)` before `join`.
  .join(', ')
array.
  map(&:to_s).join(', ')
  ^^^ Style/MapJoin: Remove redundant `map(&:to_s)` before `join`.
foo.bar.map(&:to_s).join
        ^^^ Style/MapJoin: Remove redundant `map(&:to_s)` before `join`.
[1, 2].map(&:to_s).join
       ^^^ Style/MapJoin: Remove redundant `map(&:to_s)` before `join`.
a.b.c.map { |x| x.to_s }.join("-")
      ^^^ Style/MapJoin: Remove redundant `map(&:to_s)` before `join`.
