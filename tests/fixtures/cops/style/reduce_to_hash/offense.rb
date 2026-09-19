array.each_with_object({}) { |elem, hash| hash[elem.id] = elem.name }
      ^ Style/ReduceToHash: Use `to_h { ... }` instead of `each_with_object`.
array.each_with_object({}) do |elem, hash|
      ^ Style/ReduceToHash: Use `to_h { ... }` instead of `each_with_object`.
  hash[elem.id] = elem.name
end
array&.each_with_object({}) { |elem, hash| hash[elem.id] = elem.name }
       ^ Style/ReduceToHash: Use `to_h { ... }` instead of `each_with_object`.
each_with_object({}) { |elem, hash| hash[elem] = elem.to_s }
^ Style/ReduceToHash: Use `to_h { ... }` instead of `each_with_object`.
array.each_with_object({}) { |x, h| h[x] = true }
      ^ Style/ReduceToHash: Use `to_h { ... }` instead of `each_with_object`.
array.inject({}) { |hash, elem| hash[elem.id] = elem.name; hash }
      ^ Style/ReduceToHash: Use `to_h { ... }` instead of `inject`.
array.inject({}) do |hash, elem|
      ^ Style/ReduceToHash: Use `to_h { ... }` instead of `inject`.
  hash[elem.id] = elem.name
  hash
end
array&.inject({}) { |hash, elem| hash[elem.id] = elem.name; hash }
       ^ Style/ReduceToHash: Use `to_h { ... }` instead of `inject`.
array.reduce({}) { |hash, elem| hash[elem.id] = elem.name; hash }
      ^ Style/ReduceToHash: Use `to_h { ... }` instead of `reduce`.
array.each_with_object({}) { _2[_1.id] = _1.name }
      ^ Style/ReduceToHash: Use `to_h { ... }` instead of `each_with_object`.
array.inject({}) { _1[_2.id] = _2.name; _1 }
      ^ Style/ReduceToHash: Use `to_h { ... }` instead of `inject`.
