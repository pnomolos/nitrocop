array.each_with_object(Hash.new(0)) { |item, counts| counts[item] += 2 }

array.each_with_object(Hash.new(0)) { |item, counts| counts[item.to_s] += 1 }

array.each_with_object({}) { |item, counts| counts[item] = true }

array.each_with_object(Hash.new(0)) do |item, counts|
  counts[item] += 1
  puts item
end

array.each_with_object(Hash.new(0)) { _2[_1.to_s] += 1 }

array.group_by(&:name).transform_values(&:count)

array.group_by(&:itself).transform_values(&:sum)

array.group_by { |x| x.name }.transform_values(&:count)

array.group_by { |x| x }.transform_values { |v| v.sum }

array.group_by { |x| x }.transform_values { |v| v.count * 2 }
