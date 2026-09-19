Struct.new(:foo, keyword_init: false)

Struct.new(:foo)

Struct.new(:foo, keyword_init: false, keyword_init: true)

Struct.new(:foo, keyword_init: false, keyword_init: nil)

Struct.new(:foo, some_other: true)

NotStruct.new(:foo, keyword_init: true)

Foo::Struct.new(:foo, keyword_init: true)

Struct.new({ keyword_init: true }, :foo)
