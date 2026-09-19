# nitrocop-filename: test.rb
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
      alias_method :save, :original_save
      ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Lint/DuplicateMethods: Method `FooTest::Foo#save` is defined at both test.rb:14 and test.rb:15.
    end
  end
end
