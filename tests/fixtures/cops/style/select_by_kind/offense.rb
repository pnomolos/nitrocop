array.select { |x| x.is_a?(Foo) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByKind: Prefer `grep` to `select` with a kind check.
array.filter { |x| x.kind_of?(Foo) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByKind: Prefer `grep` to `filter` with a kind check.
array.find_all { |x| x.is_a?(Foo::Bar) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByKind: Prefer `grep` to `find_all` with a kind check.
array.reject { |x| x.is_a?(Foo) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByKind: Prefer `grep_v` to `reject` with a kind check.
array.select { |x| !x.is_a?(Foo) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByKind: Prefer `grep_v` to `select` with a kind check.
array.reject { |x| !x.kind_of?(Foo) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByKind: Prefer `grep` to `reject` with a kind check.
array.select do |x|
^^^^^^^^^^^^^^^^^^^ Style/SelectByKind: Prefer `grep` to `select` with a kind check.
  x.is_a?(Foo)
end
select { |x| x.is_a?(Foo) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByKind: Prefer `grep` to `select` with a kind check.
[].select { |x| x.is_a?(Foo) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByKind: Prefer `grep` to `select` with a kind check.
foo.to_a.select { |x| x.is_a?(Foo) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByKind: Prefer `grep` to `select` with a kind check.
array.select { _1.is_a?(Foo) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByKind: Prefer `grep` to `select` with a kind check.
array&.reject { _1.is_a?(Foo) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByKind: Prefer `grep_v` to `reject` with a kind check.
array.select { !_1.kind_of?(Foo) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByKind: Prefer `grep_v` to `select` with a kind check.
array.select { it.is_a?(Foo) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByKind: Prefer `grep` to `select` with a kind check.
array.reject { !it.is_a?(Foo) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByKind: Prefer `grep` to `reject` with a kind check.
