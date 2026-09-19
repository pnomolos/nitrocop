describe "something" do
  def some_method
    implement 1
  end
  def some_method
    implement 2
  end
end

a = Class.new do
  def foo
  end
end
b = Class.new do
  def foo
  end
end

module FooTest
  def make_save_always_fail
    Foo.class_eval do
      def failed_save
        raise
      end
      alias_method :original_save, :save
      alias_method :save, :failed_save
    end

    yield
  ensure
    Foo.class_eval do
      alias_method :save, :original_save
    end
  end
end

module FooTest
  def make_save_always_fail
    Foo.class_eval do
      def failed_save
        raise
      end
      alias_method :original_save, :save
      alias_method :save, :failed_save
    end

    yield
  rescue
    Foo.class_eval do
      alias_method :save, :original_save
    end
  end
end

class A
  class << self
    def foo
    end

    class B
      def foo
      end
    end
  end
end
