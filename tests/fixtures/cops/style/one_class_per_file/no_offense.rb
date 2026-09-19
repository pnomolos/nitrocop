require 'foo'
require 'bar'

module Foo
  class Bar
  end

  class Baz
  end

  module Qux
  end
end

class << self
  def something
  end
end

if condition
  class Conditional
  end

  module Other
  end
end
