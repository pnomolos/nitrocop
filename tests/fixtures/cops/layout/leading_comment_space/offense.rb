#missing space
^ Layout/LeadingCommentSpace: Missing space after `#`.
x = 1
#bad
^ Layout/LeadingCommentSpace: Missing space after `#`.
y = 2
#another bad comment
^ Layout/LeadingCommentSpace: Missing space after `#`.

##patterns += patterns.collect(&:to_s)
^ Layout/LeadingCommentSpace: Missing space after `#`.

##$FUNCTOR_EXCEPTIONS ||= [:binding]
^ Layout/LeadingCommentSpace: Missing space after `#`.

#!self.collection_items.unrevealed.empty?
^ Layout/LeadingCommentSpace: Missing space after `#`.

#!self.collection_items.anonymous.empty?
^ Layout/LeadingCommentSpace: Missing space after `#`.

##!/usr/bin/env ruby
^ Layout/LeadingCommentSpace: Missing space after `#`.

# Some comment
#-
^^ Layout/LeadingCommentSpace: Missing space after `#`.
class Foo
end
