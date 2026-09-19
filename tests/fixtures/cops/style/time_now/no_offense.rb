# Ported from vendor/rubocop @ v1.91.0
# spec/rubocop/cop/style/time_now_spec.rb.

Time.now

# `Time.new` with arguments constructs a specific time.
Time.new(2026, 8, 19)
Time.new(in: '+09:00')

# `new` on a namespaced constant is a different class.
Foo::Time.new

# `new` on another constant.
Date.new

# `new` with no receiver at all.
new
