require 'something'

class Foo
end

module Bar
^^^^^^^^^^ Style/OneClassPerFile: Do not define multiple classes/modules at the top level in a single file.
end

class Baz::Qux
^^^^^^^^^^^^^^ Style/OneClassPerFile: Do not define multiple classes/modules at the top level in a single file.
end

class << self
  def singleton_not_counted; end
end

if condition
  class NotTopLevel
  end
end

class Last
^^^^^^^^^ Style/OneClassPerFile: Do not define multiple classes/modules at the top level in a single file.
  def method_two
  end
end
