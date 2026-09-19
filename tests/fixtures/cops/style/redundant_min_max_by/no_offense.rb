array.max_by { |x| x.foo }

array.min_by { |x| -x }

array.minmax_by(&:foo)

array.max_by { |x, y| x }

array.max_by { |x| y }

array.sort_by { |x| x }
