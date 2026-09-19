{foo: 1, bar: 2, baz: 3}.reject { |k, v| k == :bar }
                         ^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/HashExcept: Use `except(:bar)` instead.

{foo: 1, bar: 2, baz: 3}&.reject { |k, v| k == :bar }
                          ^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/HashExcept: Use `except(:bar)` instead.

{foo: 1, bar: 2, baz: 3}.reject { |k, v| :bar == k }
                         ^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/HashExcept: Use `except(:bar)` instead.

{foo: 1, bar: 2, baz: 3}.select { |k, v| k != :bar }
                         ^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/HashExcept: Use `except(:bar)` instead.

{foo: 1, bar: 2, baz: 3}.select { |k, v| :bar != k }
                         ^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/HashExcept: Use `except(:bar)` instead.

hash.reject { |k, v| k == 'str' }
     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/HashExcept: Use `except('str')` instead.

hash.reject { |k, v| k.eql?(0.0) }
     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/HashExcept: Use `except(0.0)` instead.

{foo: 1, bar: 2, baz: 3}.filter { |k, v| k != :bar }
                         ^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/HashExcept: Use `except(:bar)` instead.
