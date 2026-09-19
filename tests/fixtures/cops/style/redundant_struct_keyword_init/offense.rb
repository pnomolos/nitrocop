Struct.new(:foo, keyword_init: nil)
                 ^^^^^^^^^^^^^^^^^ Style/RedundantStructKeywordInit: Remove the redundant `keyword_init: nil`.

Struct.new(:foo, keyword_init: true)
                 ^^^^^^^^^^^^^^^^^^ Style/RedundantStructKeywordInit: Remove the redundant `keyword_init: true`.

Struct.new(a: 1, keyword_init: nil, b: 2)
                 ^^^^^^^^^^^^^^^^^ Style/RedundantStructKeywordInit: Remove the redundant `keyword_init: nil`.

Struct.new(keyword_init: true, a: 1)
           ^^^^^^^^^^^^^^^^^^ Style/RedundantStructKeywordInit: Remove the redundant `keyword_init: true`.

Struct.new(keyword_init: true)
           ^^^^^^^^^^^^^^^^^^ Style/RedundantStructKeywordInit: Remove the redundant `keyword_init: true`.

Struct&.new(:foo, keyword_init: true)
                  ^^^^^^^^^^^^^^^^^^ Style/RedundantStructKeywordInit: Remove the redundant `keyword_init: true`.

::Struct.new(:foo, keyword_init: true)
                   ^^^^^^^^^^^^^^^^^^ Style/RedundantStructKeywordInit: Remove the redundant `keyword_init: true`.

Struct.new(:foo, keyword_init: true, keyword_init: true)
                                     ^^^^^^^^^^^^^^^^^^ Style/RedundantStructKeywordInit: Remove the redundant `keyword_init: true`.
                 ^^^^^^^^^^^^^^^^^^ Style/RedundantStructKeywordInit: Remove the redundant `keyword_init: true`.

Struct.new(:foo, keyword_init: nil, keyword_init: true)
                                    ^^^^^^^^^^^^^^^^^^ Style/RedundantStructKeywordInit: Remove the redundant `keyword_init: true`.
                 ^^^^^^^^^^^^^^^^^ Style/RedundantStructKeywordInit: Remove the redundant `keyword_init: nil`.
