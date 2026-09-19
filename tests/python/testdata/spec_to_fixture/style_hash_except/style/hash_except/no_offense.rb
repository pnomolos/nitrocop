{foo: 1, bar: 2, baz: 3}.reject { |k, v| k.in?(%i[foo bar]) }

{foo: 1, bar: 2, baz: 3}&.reject { |k, v| k.in?(%i[foo bar]) }

{foo: 1, bar: 2, baz: 3}.reject { |k, v| %i[foo bar].in?(k) }

{foo: 1, bar: 2, baz: 3}.reject { |k, v| !%i[foo bar].exclude?(k) }

{foo: 1, bar: 2, baz: 3}&.reject { |k, v| !%i[foo bar].exclude?(k) }

{foo: 1, bar: 2, baz: 3}.reject { |k, v| k.exclude?('oo') }

{foo: 1, bar: 2, baz: 3}.reject { |k, v| !k.exclude?('oo') }

hash.reject { |k, v| k == 0.0 }

hash.select { |k, v| k != 0.0 }

hash.select { |k, v| !(k == 0.0) }

hash.reject { |k, v| !(k != 0.0) }

{foo: 1, bar: 2, baz: 3}.delete_if { |k, v| k == :bar }

{foo: 1, bar: 2, baz: 3}.keep_if { |k, v| k != :bar }

{foo: 1, bar: 2, baz: 3}.reject { |k, v| v.eql? :bar }

{foo: 1, bar: 2, baz: 3}.reject { |k, v, o| k == :bar }

{foo: 1, bar: 2, baz: 3}.reject { |k, v| %i[foo bar].include? }

{foo: 1, bar: 2, baz: 3}.reject { |k, v| k != :bar }

{foo: 1, bar: 2, baz: 3}.reject { |k, v| :bar != key }

{foo: 1, bar: 2, baz: 3}.select { |k, v| k == :bar }

{foo: 1, bar: 2, baz: 3}.select { |k, v| :bar == key }

{foo: 1, bar: 2, baz: 3}.reject { |k, v| do_something != :bar }

{foo: 1, bar: 2, baz: 3}.reject

{foo: 1, bar: 2, baz: 3}.except(:bar)
