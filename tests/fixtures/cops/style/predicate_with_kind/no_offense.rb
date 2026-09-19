array.any?

array.any? { |x| x.even? }

array.any? do |x|
  next if x.nil?
  x.is_a?(Integer)
end

array.any? { |x| y.is_a?(Integer) }

array.any?(Integer)

array.select { |x| x.is_a?(Integer) }

array.any? { |x, y| x.is_a?(Integer) }
