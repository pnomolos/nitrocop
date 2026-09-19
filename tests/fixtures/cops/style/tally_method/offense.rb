array.each_with_object(Hash.new(0)) { |item, counts| counts[item] += 1 }
      ^^^^^^^^^^^^^^^^ Style/TallyMethod: Use `tally` instead of `each_with_object`.

array&.each_with_object(Hash.new(0)) { |item, counts| counts[item] += 1 }
       ^^^^^^^^^^^^^^^^ Style/TallyMethod: Use `tally` instead of `each_with_object`.

array.each_with_object(Hash.new(0)) do |item, counts|
      ^^^^^^^^^^^^^^^^ Style/TallyMethod: Use `tally` instead of `each_with_object`.
  counts[item] += 1
end

each_with_object(Hash.new(0)) { |item, counts| counts[item] += 1 }
^^^^^^^^^^^^^^^^ Style/TallyMethod: Use `tally` instead of `each_with_object`.

array.each_with_object(::Hash.new(0)) { |x, h| h[x] += 1 }
      ^^^^^^^^^^^^^^^^ Style/TallyMethod: Use `tally` instead of `each_with_object`.

array.each_with_object(Hash.new(0)) { _2[_1] += 1 }
      ^^^^^^^^^^^^^^^^ Style/TallyMethod: Use `tally` instead of `each_with_object`.

array&.group_by(&:itself)&.transform_values(&:count)
       ^^^^^^^^ Style/TallyMethod: Use `tally` instead of `group_by` and `transform_values`.

group_by(&:itself).transform_values(&:count)
^^^^^^^^ Style/TallyMethod: Use `tally` instead of `group_by` and `transform_values`.

array.group_by { |x| x }.transform_values(&:count)
      ^^^^^^^^ Style/TallyMethod: Use `tally` instead of `group_by` and `transform_values`.

array.group_by { _1 }.transform_values(&:count)
      ^^^^^^^^ Style/TallyMethod: Use `tally` instead of `group_by` and `transform_values`.

array.group_by { it }.transform_values(&:count)
      ^^^^^^^^ Style/TallyMethod: Use `tally` instead of `group_by` and `transform_values`.

array.group_by(&:itself).transform_values { |v| v.count }
      ^^^^^^^^ Style/TallyMethod: Use `tally` instead of `group_by` and `transform_values`.

array.group_by(&:itself).transform_values { _1.count }
      ^^^^^^^^ Style/TallyMethod: Use `tally` instead of `group_by` and `transform_values`.

array.group_by(&:itself).transform_values { it.count }
      ^^^^^^^^ Style/TallyMethod: Use `tally` instead of `group_by` and `transform_values`.
