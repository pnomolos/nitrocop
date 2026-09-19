array.max_by { |x| x }
      ^^^^^^^^^^^^^^^^ Style/RedundantMinMaxBy: Use `max` instead of `max_by { |x| x }`.
array&.min_by { |y| y }
       ^^^^^^^^^^^^^^^^ Style/RedundantMinMaxBy: Use `min` instead of `min_by { |y| y }`.
array.minmax_by { |z| z }
      ^^^^^^^^^^^^^^^^^^^ Style/RedundantMinMaxBy: Use `minmax` instead of `minmax_by { |z| z }`.
array.max_by do |x|
      ^^^^^^^^^^^^^ Style/RedundantMinMaxBy: Use `max` instead of `max_by { |x| x }`.
  x
end
array.max_by { _1 }
      ^^^^^^^^^^^^^ Style/RedundantMinMaxBy: Use `max` instead of `max_by { _1 }`.
array&.min_by { _1 }
       ^^^^^^^^^^^^^ Style/RedundantMinMaxBy: Use `min` instead of `min_by { _1 }`.
array.minmax_by { it }
      ^^^^^^^^^^^^^^^^ Style/RedundantMinMaxBy: Use `minmax` instead of `minmax_by { it }`.
array&.max_by { it }
       ^^^^^^^^^^^^^ Style/RedundantMinMaxBy: Use `max` instead of `max_by { it }`.
