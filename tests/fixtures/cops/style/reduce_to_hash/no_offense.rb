array.each_with_object({}) do |elem, hash|
  hash[elem.id] = elem.name
  puts elem
end
hash.each_with_object({}) { |(k, v), h| h[k.to_s] = v }
array.each_with_object(Hash.new(0)) { |elem, hash| hash[elem] += 1 }
array.each_with_object({}) { |elem, hash| hash.merge!(elem => true) }
array.each_with_object({}) { |elem, hash| other[elem] = true }
array.each_with_object({}) do |elem, hash|
  hash[elem.id] = hash[elem.id].to_i + 1
end
array.each_with_object({}) do |elem, hash|
  hash[hash.size] = elem
end
array.inject({}) { |hash, elem| hash[elem] = true; other }
array.inject({}) do |hash, elem|
  puts elem
  hash[elem] = true
  hash
end
array.inject({}) do |hash, elem|
  hash[elem.id] = hash[elem.id].to_i + 1
  hash
end
array.each_with_object({}) { _2[_1.id] = _2[_1.id].to_i + 1 }
