array.any? { |x| x.is_a?(Integer) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/PredicateWithKind: Prefer `any?(Integer)` to `any? { ... }` with a kind check.
array.all? { |x| x.kind_of?(String) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/PredicateWithKind: Prefer `all?(String)` to `all? { ... }` with a kind check.
array.none? { |x| x.instance_of?(Float) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/PredicateWithKind: Prefer `none?(Float)` to `none? { ... }` with a kind check.
array.one? do |x|
^^^^^^^^^^^^^^^^^ Style/PredicateWithKind: Prefer `one?(Integer)` to `one? { ... }` with a kind check.
  x.is_a?(Integer)
end
any? { |x| x.is_a?(Integer) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/PredicateWithKind: Prefer `any?(Integer)` to `any? { ... }` with a kind check.
array.any? { |x| x.is_a?(ActiveRecord::Base) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/PredicateWithKind: Prefer `any?(ActiveRecord::Base)` to `any? { ... }` with a kind check.
array&.any? { |x| x.is_a?(Integer) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/PredicateWithKind: Prefer `any?(Integer)` to `any? { ... }` with a kind check.
array.any? { _1.is_a?(Integer) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/PredicateWithKind: Prefer `any?(Integer)` to `any? { ... }` with a kind check.
array.all? { _1.kind_of?(String) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/PredicateWithKind: Prefer `all?(String)` to `all? { ... }` with a kind check.
array.any? { it.is_a?(Integer) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/PredicateWithKind: Prefer `any?(Integer)` to `any? { ... }` with a kind check.
array.all? { it.kind_of?(String) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/PredicateWithKind: Prefer `all?(String)` to `all? { ... }` with a kind check.
