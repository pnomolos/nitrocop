positives = arr.select { |x| x > 0 }
negatives = arr.reject { |x| x < 0 }
separator_one
pos2 = arr.select { |x| x > 0 }
neg2 = arr.reject { |y| y > 0 }
separator_two
pos3 = arr.select { |x| x > 0 }
neg3 = arr.reject { _1 > 0 }
separator_three
pos4 = arr1.select { |x| x > 0 }
neg4 = arr2.reject { |x| x > 0 }
separator_four
pos5 = arr.select { |x| x > 0 }
do_something
neg5 = arr.reject { |x| x > 0 }
separator_five
pos6 = arr.select(&:positive?)
neg6 = arr.reject(&:negative?)
separator_six
pos7 = arr.select(&:positive?)
neg7 = arr.reject { |x| x.negative? }
separator_seven
a1 = arr.select { |x| x.positive? }
b1 = arr.select { |x| !x.negative? }
separator_eight
a2 = arr.select { |x| x > 0 }
b2 = arr.select { |x| x > 0 }
separator_nine
a3 = arr.reject { |x| x > 0 }
b3 = arr.reject { |x| x > 0 }
if condition
  arr.select { |x| x > 0 }
else
  arr.reject { |x| x > 0 }
end
