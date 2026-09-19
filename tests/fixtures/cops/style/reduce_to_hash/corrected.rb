array.to_h { |elem| [elem.id, elem.name] }
array.to_h do |elem|
  [elem.id, elem.name]
end
array&.to_h { |elem| [elem.id, elem.name] }
to_h { |elem| [elem, elem.to_s] }
array.to_h { |x| [x, true] }
array.to_h { |elem| [elem.id, elem.name] }
array.to_h do |elem|
  [elem.id, elem.name]
end
array&.to_h { |elem| [elem.id, elem.name] }
array.to_h { |elem| [elem.id, elem.name] }
array.to_h { [_1.id, _1.name] }
array.to_h { [_1.id, _1.name] }
